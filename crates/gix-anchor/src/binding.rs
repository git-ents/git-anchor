//! [`Binding`]: the single typed-reference vocabulary into the object
//! graph — history-bound, content-bound, transformation-bound,
//! position-bound, or relational — plus read-time [`revalidate`] of a
//! binding against a revision under evaluation.
//!
//! [`Binding::Position`] is [`crate::Anchor`] unchanged: every anchor is a
//! binding, but not every binding is an anchor. The other four variants
//! name a target without a line-level position at all.
//!
//! Every variant splits into two sibling subtrees, `identity` and `hints`:
//! `identity` holds only non-derivable coordinates, `hints` holds retained,
//! versioned, or advisory material that never bears on identity. Derived
//! [`PartialEq`] on `Binding` is full structural equality (`identity` *and*
//! `hints`); two bindings with equal `identity` and different `hints` are
//! equal targets but not structurally equal — compare the `identity`
//! subtree directly for that.
//!
//! Every binding carries at least one *witness* — a commit whose ancestry
//! reaches the bound object(s) — so a claim's ledger commit can carry the
//! witness as an extra parent and keep the bound objects reachable. Every
//! witness lives in `hints`.
//!
//! `Binding` derives `facet::Facet`, so [`Binding::serialize_into`] and
//! [`Binding::deserialize`] round-trip it, via a [`crate::CaptureHandle`],
//! through `facet-git-tree`'s externally-tagged enum encoding directly.

use facet::Facet;
use gix::ObjectId;
use gix_object::{Find, Write};

use crate::anchor::Anchor;
use crate::error::{Error, Result};
use crate::handle::CaptureHandle;
use crate::oid::Oid;
use crate::projection::{Projection, project};
use crate::util::resolve_commit;

/// [`Binding::Commit`]'s identity: the commit itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
pub struct CommitIdentity {
    /// The commit named by this binding.
    pub commit: Oid,
}

/// [`Binding::Hybrid`]'s identity: a parent commit plus a body tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
pub struct HybridIdentity {
    /// The parent commit.
    pub commit: Oid,
    /// The body tree.
    pub tree: Oid,
}

/// No stored hints. `facet-git-tree` writes every field present as a
/// sentinel, so this costs nothing and adding one later is not a migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
pub struct NoHints {}

/// [`Binding::Tree`]'s identity: the tree alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
pub struct TreeIdentity {
    /// The bound tree's identity.
    pub tree: Oid,
}

/// [`Binding::Tree`]'s retained provenance — advisory, never identity-bearing.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TreeHints {
    /// Where `tree` was found when this binding was made, retained for
    /// display only.
    pub path: String,
    /// A commit whose ancestry reaches `tree`.
    pub witness: Oid,
}

/// [`Binding::Delta`]'s identity: the `(base_tree, head_tree)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
pub struct DeltaIdentity {
    /// The tree before the transformation.
    pub base_tree: Oid,
    /// The tree after the transformation.
    pub head_tree: Oid,
}

/// [`Binding::Delta`]'s retained provenance — advisory, never identity-bearing.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct DeltaHints {
    /// Where the transformation was found when this binding was made,
    /// retained for display only.
    pub path: String,
    /// A commit whose ancestry reaches `base_tree`.
    pub base_witness: Oid,
    /// A commit whose ancestry reaches `head_tree`.
    pub head_witness: Oid,
}

/// The single typed-reference vocabulary into the object graph: what a
/// claim, comment, or review is *about*.
///
/// # Examples
///
/// ```
/// use gix_anchor::{Binding, CommitIdentity, NoHints};
///
/// let commit = gix::ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567").unwrap();
/// let binding = Binding::Commit {
///     identity: CommitIdentity { commit: commit.into() },
///     hints: NoHints {},
/// };
/// assert_eq!(binding.witnesses(), vec![commit]);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum Binding {
    /// History-bound: the commit itself is the target. Its own witness is
    /// itself.
    Commit {
        /// Non-derivable coordinates.
        identity: CommitIdentity,
        /// No retained material.
        hints: NoHints,
    },
    /// Content-bound: a tree, independent of any particular commit or path.
    Tree {
        /// Non-derivable coordinates.
        identity: TreeIdentity,
        /// Retained provenance.
        hints: TreeHints,
    },
    /// Transformation-bound: the pair `(base_tree, head_tree)` — an edit,
    /// not either endpoint alone. A commit range is *evidence for* a
    /// `Delta`; it is never a binding itself.
    Delta {
        /// Non-derivable coordinates.
        identity: DeltaIdentity,
        /// Retained provenance.
        hints: DeltaHints,
    },
    /// Position-bound: [`Anchor`] verbatim — a durable pointer to specific
    /// lines (or a whole file), retained and projectable exactly as
    /// [`crate::project`] describes.
    Position(Anchor),
    /// Relational: a parent commit plus a body tree, bound as a pair
    /// distinct from either [`Binding::Commit`] or [`Binding::Tree`] alone.
    Hybrid {
        /// Non-derivable coordinates.
        identity: HybridIdentity,
        /// No retained material.
        hints: NoHints,
    },
}

impl Binding {
    /// Every commit whose ancestry must reach the bound object(s) for this
    /// binding to stay alive — never empty: the commit itself for
    /// [`Binding::Commit`] and [`Binding::Hybrid`], the anchor's own genesis
    /// commit for [`Binding::Position`], the recorded witness for
    /// [`Binding::Tree`], and both witnesses for [`Binding::Delta`].
    ///
    /// # Examples
    ///
    /// ```
    /// use gix_anchor::{Binding, DeltaHints, DeltaIdentity};
    ///
    /// let base_witness = gix::ObjectId::from_hex(b"1111111111111111111111111111111111111111").unwrap();
    /// let head_witness = gix::ObjectId::from_hex(b"2222222222222222222222222222222222222222").unwrap();
    /// let binding = Binding::Delta {
    ///     identity: DeltaIdentity {
    ///         base_tree: gix::ObjectId::from_hex(b"3333333333333333333333333333333333333333").unwrap().into(),
    ///         head_tree: gix::ObjectId::from_hex(b"4444444444444444444444444444444444444444").unwrap().into(),
    ///     },
    ///     hints: DeltaHints {
    ///         path: "src/lib.rs".to_owned(),
    ///         base_witness: base_witness.into(),
    ///         head_witness: head_witness.into(),
    ///     },
    /// };
    /// assert_eq!(binding.witnesses(), vec![base_witness, head_witness]);
    /// ```
    #[must_use]
    pub fn witnesses(&self) -> Vec<ObjectId> {
        match self {
            Self::Commit { identity, .. } => vec![ObjectId::from(identity.commit)],
            Self::Hybrid { identity, .. } => vec![ObjectId::from(identity.commit)],
            Self::Tree { hints, .. } => vec![ObjectId::from(hints.witness)],
            Self::Delta { hints, .. } => vec![
                ObjectId::from(hints.base_witness),
                ObjectId::from(hints.head_witness),
            ],
            Self::Position(anchor) => vec![ObjectId::from(anchor.identity.genesis)],
        }
    }

    /// The primary target object id — what this binding is *about*, and the
    /// grouping key a ref-based note store files entries under: the commit
    /// for [`Binding::Commit`], the tree for [`Binding::Tree`], the tree
    /// *after* the transformation for [`Binding::Delta`], the anchored blob
    /// for [`Binding::Position`], and the commit for [`Binding::Hybrid`].
    ///
    /// # Examples
    ///
    /// ```
    /// use gix_anchor::{Binding, CommitIdentity, NoHints};
    ///
    /// let commit = gix::ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567").unwrap();
    /// let binding = Binding::Commit {
    ///     identity: CommitIdentity { commit: commit.into() },
    ///     hints: NoHints {},
    /// };
    /// assert_eq!(binding.target(), commit);
    /// ```
    #[must_use]
    pub fn target(&self) -> ObjectId {
        match self {
            Self::Commit { identity, .. } => ObjectId::from(identity.commit),
            Self::Hybrid { identity, .. } => ObjectId::from(identity.commit),
            Self::Tree { identity, .. } => ObjectId::from(identity.tree),
            Self::Delta { identity, .. } => ObjectId::from(identity.head_tree),
            Self::Position(anchor) => ObjectId::from(anchor.hints.blob),
        }
    }

    /// Write `self` into `store` as an externally-tagged
    /// [`facet_git_tree`]-encoded tree, returning the [`CaptureHandle`] that
    /// locates it — not itself identity-bearing; read [`CaptureHandle::anchor_id`]
    /// for the value that is.
    ///
    /// `store` takes the same bound `facet_git_tree::serialize_into` does:
    /// any `gix` object-write sink — a real repository's object database,
    /// an in-memory [`facet_git_tree::ObjectStore`], or any other
    /// `gix_object::Write` implementation.
    ///
    /// # Examples
    ///
    /// ```
    /// use gix_anchor::{Binding, CommitIdentity, NoHints};
    /// use facet_git_tree::ObjectStore;
    ///
    /// let store = ObjectStore::default();
    /// let binding = Binding::Commit {
    ///     identity: CommitIdentity {
    ///         commit: gix::ObjectId::from_hex(b"8888888888888888888888888888888888888888").unwrap().into(),
    ///     },
    ///     hints: NoHints {},
    /// };
    /// let handle = binding.serialize_into(&store).expect("serialize");
    /// let back = Binding::deserialize(&handle, &store).expect("deserialize");
    /// assert_eq!(back, binding);
    /// ```
    ///
    /// # Errors
    ///
    /// [`Error::Serialize`] when the underlying `facet-git-tree` write fails
    /// (a backend error from `store`).
    pub fn serialize_into<W>(&self, store: &W) -> Result<CaptureHandle>
    where
        W: Write + ?Sized,
    {
        let oid = facet_git_tree::serialize_into(self, store)?;
        Ok(CaptureHandle::from(oid))
    }

    /// Read the [`Binding`] a [`CaptureHandle`] locates in `store`.
    ///
    /// `store` takes the same bound `facet_git_tree::deserialize` does:
    /// any `gix` object-read source.
    ///
    /// # Errors
    ///
    /// [`Error::Deserialize`] when `handle` does not decode as a [`Binding`].
    pub fn deserialize<F>(handle: &CaptureHandle, store: &F) -> Result<Self>
    where
        F: Find + ?Sized,
    {
        Ok(facet_git_tree::deserialize(
            &ObjectId::from(*handle),
            store,
        )?)
    }
}

/// How up to date a [`Binding`] is as of the revision [`EvalState`]
/// describes, as computed by [`revalidate`].
///
/// # Examples
///
/// ```
/// use gix_anchor::Validity;
///
/// assert_ne!(Validity::Valid, Validity::Stale);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Validity {
    /// The binding's target is still present as of the state under
    /// evaluation.
    Valid,
    /// The binding's target no longer holds as of the state under
    /// evaluation, though the check itself completed.
    Stale,
    /// Whether the binding still holds could not be determined — an
    /// unresolvable revision, a missing object, or (for a
    /// [`Binding::Delta`]) no delta pair supplied to check against.
    Unknown,
}

/// The minimum state [`revalidate`] needs beyond the [`Binding`] itself: the
/// revision every variant but [`Binding::Delta`] is checked against, plus
/// the tree pair a [`Binding::Delta`] is checked against.
///
/// # Examples
///
/// ```
/// use gix_anchor::EvalState;
///
/// let state = EvalState { at: "HEAD", delta: None };
/// assert_eq!(state.at, "HEAD");
/// ```
#[derive(Debug, Clone, Copy)]
pub struct EvalState<'a> {
    /// The revision (hex id, ref name, or revspec) the binding is being
    /// evaluated against.
    pub at: &'a str,
    /// The `(base_tree, head_tree)` pair a [`Binding::Delta`] is being
    /// evaluated against — irrelevant to every other variant.
    pub delta: Option<(ObjectId, ObjectId)>,
}

/// Check `binding`'s [`Validity`] against `state`. Per-variant semantics are
/// documented on each helper this dispatches to: [`commit_validity`],
/// [`tree_validity`], [`delta_validity`], [`position_validity`]; a
/// [`Binding::Hybrid`] is [`combine`]'s composition of its commit and tree
/// halves.
///
/// # Errors
///
/// Propagates a [`crate::project`] error other than an unresolvable
/// revision (which becomes [`Validity::Unknown`] instead, since it means
/// the state under evaluation could not be evaluated at all, not that the
/// binding itself is broken), and any I/O or decode error surfaced while
/// walking `state.at`'s tree for a [`Binding::Tree`] or [`Binding::Hybrid`]
/// check.
///
/// # Examples
///
/// ```
/// use gix_anchor::{Binding, CommitIdentity, EvalState, NoHints, Validity};
///
/// # let dir = tempfile::tempdir().expect("tempdir");
/// # std::process::Command::new("git").arg("init").arg("-q").arg(dir.path()).status().unwrap();
/// # std::fs::write(dir.path().join("file.txt"), "a\n").unwrap();
/// # std::process::Command::new("git").arg("-C").arg(dir.path()).args(["add", "-A"]).status().unwrap();
/// # std::process::Command::new("git").arg("-C").arg(dir.path())
/// #     .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "-m", "one"])
/// #     .status().unwrap();
/// let repo = gix::open(dir.path()).expect("open");
/// let commit = repo.head_id().expect("head").detach();
/// let binding = Binding::Commit {
///     identity: CommitIdentity { commit: commit.into() },
///     hints: NoHints {},
/// };
/// let state = EvalState { at: "HEAD", delta: None };
/// assert_eq!(gix_anchor::revalidate(&repo, &binding, &state).unwrap(), Validity::Valid);
/// ```
pub fn revalidate(
    repo: &gix::Repository,
    binding: &Binding,
    state: &EvalState<'_>,
) -> Result<Validity> {
    match binding {
        Binding::Commit { identity, .. } => Ok(commit_validity(
            repo,
            ObjectId::from(identity.commit),
            state.at,
        )),
        Binding::Tree { identity, hints } => {
            tree_validity(repo, ObjectId::from(identity.tree), &hints.path, state.at)
        }
        Binding::Delta { identity, .. } => Ok(delta_validity(
            ObjectId::from(identity.base_tree),
            ObjectId::from(identity.head_tree),
            state.delta,
        )),
        Binding::Position(anchor) => position_validity(repo, anchor, state.at),
        Binding::Hybrid { identity, .. } => {
            let commit_v = commit_validity(repo, ObjectId::from(identity.commit), state.at);
            let tree_v = tree_reachable(repo, ObjectId::from(identity.tree), state.at)?;
            Ok(combine(commit_v, tree_v))
        }
    }
}

/// [`Validity::Unknown`] if either input is; [`Validity::Valid`] iff both
/// are; [`Validity::Stale`] otherwise — [`Binding::Hybrid`]'s composition of
/// its commit check and its tree check.
fn combine(a: Validity, b: Validity) -> Validity {
    if a == Validity::Unknown || b == Validity::Unknown {
        Validity::Unknown
    } else if a == Validity::Valid && b == Validity::Valid {
        Validity::Valid
    } else {
        Validity::Stale
    }
}

/// [`Binding::Commit`]'s (and [`Binding::Hybrid`]'s commit half's)
/// [`Validity`]: [`Validity::Unknown`] when `commit` is absent from the odb
/// or `at` cannot be resolved, else [`Validity::Valid`] iff `commit` is `at`
/// itself or one of its ancestors (via the repository's own merge-base
/// machinery, the same idiom a consuming crate would use for review-target
/// ancestry), else [`Validity::Stale`].
fn commit_validity(repo: &gix::Repository, commit: ObjectId, at: &str) -> Validity {
    if !repo.has_object(commit) {
        return Validity::Unknown;
    }
    let Ok(target) = resolve_commit(repo, at) else {
        return Validity::Unknown;
    };
    let target_id = target.id().detach();
    if commit == target_id
        || repo
            .merge_base(commit, target_id)
            .is_ok_and(|base| base.detach() == commit)
    {
        Validity::Valid
    } else {
        Validity::Stale
    }
}

/// [`Binding::Tree`]'s [`Validity`]: the fast path (`tree` at `path` in
/// `at`'s own tree) first, falling back to [`tree_reachable`]'s recursive
/// anywhere-in-the-tree search.
fn tree_validity(repo: &gix::Repository, tree: ObjectId, path: &str, at: &str) -> Result<Validity> {
    let Ok(commit) = resolve_commit(repo, at) else {
        return Ok(Validity::Unknown);
    };
    let root = commit
        .tree()
        .map_err(|error| Error::Object(error.to_string()))?;
    if let Ok(Some(entry)) = root.lookup_entry_by_path(path)
        && entry.mode().is_tree()
        && entry.object_id() == tree
    {
        return Ok(Validity::Valid);
    }
    if tree_contains(&root, tree)? {
        Ok(Validity::Valid)
    } else {
        Ok(Validity::Stale)
    }
}

/// [`Binding::Hybrid`]'s tree-half [`Validity`]: [`tree_validity`] without a
/// recorded path to try as a fast path first — [`Binding::Hybrid`] carries
/// none.
fn tree_reachable(repo: &gix::Repository, tree: ObjectId, at: &str) -> Result<Validity> {
    let Ok(commit) = resolve_commit(repo, at) else {
        return Ok(Validity::Unknown);
    };
    let root = commit
        .tree()
        .map_err(|error| Error::Object(error.to_string()))?;
    if tree_contains(&root, tree)? {
        Ok(Validity::Valid)
    } else {
        Ok(Validity::Stale)
    }
}

/// Whether `target` is `tree` itself or the id of any subtree reachable
/// from it, at any depth — git trees form a DAG with no cycles (an entry
/// cannot name its own not-yet-written parent by content-addressed id), so
/// this recursion terminates on any well-formed tree with no explicit depth
/// guard needed.
fn tree_contains(tree: &gix::Tree<'_>, target: ObjectId) -> Result<bool> {
    if tree.id() == target {
        return Ok(true);
    }
    for entry in tree.iter() {
        let entry = entry.map_err(|error| Error::Object(error.to_string()))?;
        if !entry.mode().is_tree() {
            continue;
        }
        if entry.object_id() == target {
            return Ok(true);
        }
        let subtree = entry
            .object()
            .map_err(|error| Error::Object(error.to_string()))?
            .try_into_tree()
            .map_err(|error| Error::Object(error.to_string()))?;
        if tree_contains(&subtree, target)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// [`Binding::Delta`]'s [`Validity`]: identity comparison against
/// `state.delta` only, per `revalidate`'s spec — no repository access at
/// all, since a `Delta`'s evidence (the tree pair under evaluation) is
/// caller context, not something derivable from a single revision.
fn delta_validity(
    base_tree: ObjectId,
    head_tree: ObjectId,
    delta: Option<(ObjectId, ObjectId)>,
) -> Validity {
    match delta {
        Some(pair) if pair == (base_tree, head_tree) => Validity::Valid,
        Some(_) => Validity::Stale,
        None => Validity::Unknown,
    }
}

/// [`Binding::Position`]'s [`Validity`]: [`crate::project`]'s four outcomes
/// collapsed to three, with an unresolvable `at` reported as
/// [`Validity::Unknown`] rather than propagated — every other
/// [`crate::project`] error is a clearer sign of a broken anchor than of an
/// unevaluable state, so those propagate.
fn position_validity(repo: &gix::Repository, anchor: &Anchor, at: &str) -> Result<Validity> {
    match project(repo, anchor, at) {
        Ok(Projection::Current | Projection::Relocated { .. }) => Ok(Validity::Valid),
        Ok(Projection::Outdated { .. } | Projection::Deleted) => Ok(Validity::Stale),
        Err(Error::Resolve(_)) => Ok(Validity::Unknown),
        Err(other) => Err(other),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "unit test"
    )]

    use facet_git_tree::ObjectStore;
    use rstest::rstest;

    use super::*;
    use crate::LineRange;
    use crate::anchor::capture;
    use crate::fixture::{commit_all, numbered, repo};

    fn hex(byte: u8) -> ObjectId {
        let hex_digit = format!("{byte:x}");
        let full = hex_digit.repeat(40);
        ObjectId::from_hex(full.as_bytes()).unwrap()
    }

    fn sample_tree() -> Binding {
        Binding::Tree {
            identity: TreeIdentity {
                tree: hex(1).into(),
            },
            hints: TreeHints {
                path: "src/lib.rs".to_owned(),
                witness: hex(2).into(),
            },
        }
    }

    fn sample_delta() -> Binding {
        Binding::Delta {
            identity: DeltaIdentity {
                base_tree: hex(3).into(),
                head_tree: hex(4).into(),
            },
            hints: DeltaHints {
                path: "src/lib.rs".to_owned(),
                base_witness: hex(5).into(),
                head_witness: hex(6).into(),
            },
        }
    }

    fn sample_commit() -> Binding {
        Binding::Commit {
            identity: CommitIdentity {
                commit: hex(7).into(),
            },
            hints: NoHints {},
        }
    }

    fn sample_hybrid() -> Binding {
        Binding::Hybrid {
            identity: HybridIdentity {
                commit: hex(8).into(),
                tree: hex(9).into(),
            },
            hints: NoHints {},
        }
    }

    fn sample_position() -> Binding {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=5)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(&git_repo, "HEAD", "file.txt", None).unwrap();
        Binding::Position(anchor)
    }

    #[rstest]
    #[case::commit(sample_commit())]
    #[case::tree(sample_tree())]
    #[case::delta(sample_delta())]
    #[case::position(sample_position())]
    #[case::hybrid(sample_hybrid())]
    fn every_variant_round_trips_through_serialize_and_deserialize(#[case] binding: Binding) {
        let store = ObjectStore::default();
        let handle = binding.serialize_into(&store).expect("serialize");
        let back = Binding::deserialize(&handle, &store).expect("deserialize");
        assert_eq!(back, binding);
    }

    #[test]
    fn deserialize_rejects_a_tree_that_is_not_a_tagged_variant() {
        let store = ObjectStore::default();
        let root = gix_object::Write::write(&store, &gix_object::Tree { entries: vec![] }).unwrap();
        let handle = crate::handle::CaptureHandle::from(root);
        let error = Binding::deserialize(&handle, &store).unwrap_err();
        assert!(matches!(error, Error::Deserialize(_)));
    }

    #[rstest]
    #[case::commit(sample_commit(), vec![hex(7)])]
    #[case::tree(sample_tree(), vec![hex(2)])]
    #[case::delta(sample_delta(), vec![hex(5), hex(6)])]
    #[case::hybrid(sample_hybrid(), vec![hex(8)])]
    fn witnesses_are_never_empty_and_match_the_spec(
        #[case] binding: Binding,
        #[case] expected: Vec<ObjectId>,
    ) {
        assert_eq!(binding.witnesses(), expected);
        assert!(!binding.witnesses().is_empty());
    }

    #[test]
    fn witnesses_of_a_position_is_the_anchors_own_genesis_commit() {
        let binding = sample_position();
        let Binding::Position(anchor) = &binding else {
            panic!("sample_position must build a Position");
        };
        assert_eq!(
            binding.witnesses(),
            vec![ObjectId::from(anchor.identity.genesis)]
        );
    }

    /// `anchor.identity`: identity equality is now a direct comparison of
    /// the `identity` subtree, not a hand-written per-variant predicate.
    #[test]
    fn identity_equality_ignores_hints_for_tree() {
        let a = sample_tree();
        let Binding::Tree { identity: ia, .. } = &a else {
            panic!("sample_tree must build a Tree");
        };
        let b = Binding::Tree {
            identity: *ia,
            hints: TreeHints {
                path: "different.rs".to_owned(),
                witness: hex(9).into(),
            },
        };
        assert_ne!(a, b, "differing hints: not structurally equal");
        let Binding::Tree { identity: ib, .. } = &b else {
            unreachable!()
        };
        assert_eq!(ia, ib, "same identity: same target regardless of hints");
    }

    #[test]
    fn identity_equality_ignores_hints_for_delta() {
        let a = sample_delta();
        let Binding::Delta { identity: ia, .. } = &a else {
            panic!("sample_delta must build a Delta");
        };
        let b = Binding::Delta {
            identity: *ia,
            hints: DeltaHints {
                path: "different.rs".to_owned(),
                base_witness: hex(1).into(),
                head_witness: hex(2).into(),
            },
        };
        assert_ne!(a, b);
        let Binding::Delta { identity: ib, .. } = &b else {
            unreachable!()
        };
        assert_eq!(ia, ib);
    }

    #[test]
    fn different_variants_are_never_structurally_equal() {
        assert_ne!(sample_commit(), sample_hybrid());
    }

    #[test]
    fn position_identity_ignores_hints_and_varies_with_any_coordinate() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(
            &git_repo,
            "HEAD",
            "file.txt",
            Some(LineRange { start: 3, end: 4 }),
        )
        .unwrap();

        let mut same_identity = anchor.clone();
        same_identity.hints.context = b"unrelated\n".to_vec();
        assert_eq!(anchor.identity, same_identity.identity);
        assert_ne!(
            Binding::Position(anchor.clone()),
            Binding::Position(same_identity),
            "differing hints: not structurally equal"
        );

        std::fs::write(dir.path().join("file.txt"), numbered(1..=12)).unwrap();
        commit_all(dir.path(), "two");
        let git_repo = gix::open(dir.path()).unwrap();
        let other = capture(
            &git_repo,
            "HEAD",
            "file.txt",
            Some(LineRange { start: 3, end: 4 }),
        )
        .unwrap();
        assert_ne!(anchor.identity, other.identity);
    }

    #[test]
    fn revalidate_commit_is_valid_for_self_and_ancestor_and_stale_otherwise() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), "one\n").unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let first = git_repo.head_id().unwrap().detach();

        std::fs::write(dir.path().join("file.txt"), "two\n").unwrap();
        commit_all(dir.path(), "two");
        let git_repo = gix::open(dir.path()).unwrap();
        let second = git_repo.head_id().unwrap().detach();

        let state = EvalState {
            at: "HEAD",
            delta: None,
        };
        assert_eq!(
            revalidate(
                &git_repo,
                &Binding::Commit {
                    identity: CommitIdentity {
                        commit: second.into()
                    },
                    hints: NoHints {},
                },
                &state
            )
            .unwrap(),
            Validity::Valid
        );
        assert_eq!(
            revalidate(
                &git_repo,
                &Binding::Commit {
                    identity: CommitIdentity {
                        commit: first.into()
                    },
                    hints: NoHints {},
                },
                &state
            )
            .unwrap(),
            Validity::Valid,
            "an ancestor of the revision under evaluation is still valid"
        );

        // A commit that only exists on an unrelated, unmerged branch is
        // neither `second` nor an ancestor of it — evaluated against
        // `second` explicitly, since `HEAD` itself is about to move to the
        // unrelated branch.
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["checkout", "-q", "--orphan", "other"])
            .status()
            .unwrap();
        std::fs::write(dir.path().join("other.txt"), "other\n").unwrap();
        commit_all(dir.path(), "unrelated");
        let git_repo = gix::open(dir.path()).unwrap();
        let unrelated = git_repo.head_id().unwrap().detach();

        let second_hex = second.to_string();
        let state_at_second = EvalState {
            at: &second_hex,
            delta: None,
        };
        assert_eq!(
            revalidate(
                &git_repo,
                &Binding::Commit {
                    identity: CommitIdentity {
                        commit: unrelated.into()
                    },
                    hints: NoHints {},
                },
                &state_at_second
            )
            .unwrap(),
            Validity::Stale
        );
    }

    #[test]
    fn revalidate_commit_is_unknown_when_absent_or_revision_unresolvable() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), "one\n").unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();

        let missing = Binding::Commit {
            identity: CommitIdentity {
                commit: gix::ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567")
                    .unwrap()
                    .into(),
            },
            hints: NoHints {},
        };
        let state = EvalState {
            at: "HEAD",
            delta: None,
        };
        assert_eq!(
            revalidate(&git_repo, &missing, &state).unwrap(),
            Validity::Unknown
        );

        let head = Binding::Commit {
            identity: CommitIdentity {
                commit: git_repo.head_id().unwrap().detach().into(),
            },
            hints: NoHints {},
        };
        let unresolvable = EvalState {
            at: "not-a-revision",
            delta: None,
        };
        assert_eq!(
            revalidate(&git_repo, &head, &unresolvable).unwrap(),
            Validity::Unknown
        );
    }

    #[test]
    fn revalidate_tree_checks_the_recorded_path_then_falls_back_to_any_path() {
        let dir = repo();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/file.txt"), "one\n").unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let commit = git_repo.head_id().unwrap().detach();
        let root = git_repo.find_commit(commit).unwrap().tree().unwrap();
        let sub_tree = root
            .lookup_entry_by_path("sub")
            .unwrap()
            .unwrap()
            .object_id();

        let state = EvalState {
            at: "HEAD",
            delta: None,
        };

        // Fast path: recorded at its real path.
        let at_path = Binding::Tree {
            identity: TreeIdentity {
                tree: sub_tree.into(),
            },
            hints: TreeHints {
                path: "sub".to_owned(),
                witness: commit.into(),
            },
        };
        assert_eq!(
            revalidate(&git_repo, &at_path, &state).unwrap(),
            Validity::Valid
        );

        // Anywhere fallback: recorded at a wrong path, but the same tree
        // still sits somewhere in the target's tree.
        let wrong_path = Binding::Tree {
            identity: TreeIdentity {
                tree: sub_tree.into(),
            },
            hints: TreeHints {
                path: "not/the/real/path".to_owned(),
                witness: commit.into(),
            },
        };
        assert_eq!(
            revalidate(&git_repo, &wrong_path, &state).unwrap(),
            Validity::Valid
        );

        let missing = Binding::Tree {
            identity: TreeIdentity {
                tree: gix::ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567")
                    .unwrap()
                    .into(),
            },
            hints: TreeHints {
                path: "sub".to_owned(),
                witness: commit.into(),
            },
        };
        assert_eq!(
            revalidate(&git_repo, &missing, &state).unwrap(),
            Validity::Stale
        );
    }

    #[test]
    fn revalidate_tree_is_unknown_when_the_revision_is_unresolvable() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), "one\n").unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let binding = sample_tree();
        let state = EvalState {
            at: "not-a-revision",
            delta: None,
        };
        assert_eq!(
            revalidate(&git_repo, &binding, &state).unwrap(),
            Validity::Unknown
        );
    }

    #[rstest]
    #[case::matching_pair(Some((hex(3), hex(4))), Validity::Valid)]
    #[case::different_pair(Some((hex(1), hex(2))), Validity::Stale)]
    #[case::no_pair(None, Validity::Unknown)]
    fn revalidate_delta_compares_identity_against_state_delta_only(
        #[case] delta: Option<(ObjectId, ObjectId)>,
        #[case] expected: Validity,
    ) {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), "one\n").unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();

        let binding = sample_delta();
        let state = EvalState { at: "HEAD", delta };
        assert_eq!(revalidate(&git_repo, &binding, &state).unwrap(), expected);
    }

    #[test]
    fn revalidate_position_maps_current_and_relocated_to_valid() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(&git_repo, "HEAD", "file.txt", None).unwrap();
        let binding = Binding::Position(anchor);
        let state = EvalState {
            at: "HEAD",
            delta: None,
        };
        assert_eq!(
            revalidate(&git_repo, &binding, &state).unwrap(),
            Validity::Valid
        );
    }

    #[test]
    fn revalidate_position_maps_deleted_to_stale() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(&git_repo, "HEAD", "file.txt", None).unwrap();

        std::fs::remove_file(dir.path().join("file.txt")).unwrap();
        std::fs::write(dir.path().join("other.txt"), "x\n").unwrap();
        commit_all(dir.path(), "two");
        let git_repo = gix::open(dir.path()).unwrap();

        let binding = Binding::Position(anchor);
        let state = EvalState {
            at: "HEAD",
            delta: None,
        };
        assert_eq!(
            revalidate(&git_repo, &binding, &state).unwrap(),
            Validity::Stale
        );
    }

    #[test]
    fn revalidate_position_is_unknown_when_the_revision_is_unresolvable() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(&git_repo, "HEAD", "file.txt", None).unwrap();

        let binding = Binding::Position(anchor);
        let state = EvalState {
            at: "not-a-revision",
            delta: None,
        };
        assert_eq!(
            revalidate(&git_repo, &binding, &state).unwrap(),
            Validity::Unknown
        );
    }

    #[test]
    fn revalidate_hybrid_is_valid_iff_both_commit_and_tree_check_out() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), "one\n").unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let commit = git_repo.head_id().unwrap().detach();
        let tree = git_repo
            .find_commit(commit)
            .unwrap()
            .tree()
            .unwrap()
            .id()
            .detach();

        let state = EvalState {
            at: "HEAD",
            delta: None,
        };
        let valid = Binding::Hybrid {
            identity: HybridIdentity {
                commit: commit.into(),
                tree: tree.into(),
            },
            hints: NoHints {},
        };
        assert_eq!(
            revalidate(&git_repo, &valid, &state).unwrap(),
            Validity::Valid
        );

        let stale_tree = Binding::Hybrid {
            identity: HybridIdentity {
                commit: commit.into(),
                tree: gix::ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567")
                    .unwrap()
                    .into(),
            },
            hints: NoHints {},
        };
        assert_eq!(
            revalidate(&git_repo, &stale_tree, &state).unwrap(),
            Validity::Stale
        );

        let unknown_commit = Binding::Hybrid {
            identity: HybridIdentity {
                commit: gix::ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567")
                    .unwrap()
                    .into(),
                tree: tree.into(),
            },
            hints: NoHints {},
        };
        assert_eq!(
            revalidate(&git_repo, &unknown_commit, &state).unwrap(),
            Validity::Unknown
        );
    }
}
