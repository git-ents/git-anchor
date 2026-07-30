//! [`Anchor`]: identity, retained hints, capture, and the anchor id.
//!
//! Spec coverage: `anchor.definition`, `anchor.identity`, `anchor.immutable`,
//! `anchor.retention`.

use std::collections::BTreeMap;

use facet::Facet;
use facet_git_tree::normal_form::NormalForm;

use crate::error::{Error, Result};
use crate::fingerprint::{Fingerprint, capture_fingerprint};
use crate::handle::{AnchorId, IdentityNormalForm};
use crate::oid::Oid;
use crate::util::{byte_span_of, read_blob, resolve_commit};

/// A 1-based inclusive range of lines, as a user supplies one to
/// [`crate::capture`]/[`crate::capture_worktree`] (`git anchor create -L`).
///
/// Capture-time canonicalization is legal (ARCHITECTURE.md, "Identity
/// rule"): this exists only to be converted to a [`Span`] during capture. It
/// never appears in [`AnchorIdentity`] and is not itself durable.
///
/// # Examples
///
/// ```
/// use gix_anchor::LineRange;
///
/// let range = LineRange { start: 3, end: 4 };
/// assert_eq!(range.end - range.start + 1, 2, "two lines, inclusive");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRange {
    /// The first line of the range, 1-based.
    pub start: u64,
    /// The last line of the range, inclusive.
    pub end: u64,
}

/// A half-open byte range over a blob's bytes exactly as git stores them
/// (post-clean-filter) — the one canonical form [`AnchorIdentity`] carries
/// (ARCHITECTURE.md, "git-anchor"). `start == end` names an empty span at
/// that offset; `start == 0 && end == len` names the whole blob.
///
/// # Examples
///
/// ```
/// use gix_anchor::Span;
///
/// let span = Span { start: 10, end: 20 };
/// assert_eq!(span.end - span.start, 10);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
pub struct Span {
    /// The span's first byte offset, inclusive.
    pub start: u64,
    /// The span's last byte offset, exclusive.
    pub end: u64,
}

impl Span {
    /// The whole of a blob `len` bytes long.
    fn whole(len: usize) -> Self {
        Self {
            start: 0,
            end: u64::try_from(len).unwrap_or(u64::MAX),
        }
    }

    /// `bytes[self]`, or `None` when `self` does not fit `bytes`.
    #[must_use]
    pub fn slice<'b>(&self, bytes: &'b [u8]) -> Option<&'b [u8]> {
        let start = usize::try_from(self.start).ok()?;
        let end = usize::try_from(self.end).ok()?;
        bytes.get(start..end)
    }
}

/// An anchor's non-derivable coordinates (`anchor.definition`): the commit it
/// was captured against, its path, and its byte span. Nothing else belongs
/// here — [`Anchor::id`] is the content hash of this alone (`anchor.identity`),
/// routed through the identity normal form (`crate::handle::hash_identity`)
/// rather than the general codec.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[facet(facet_git_tree::identity_key)]
pub struct AnchorIdentity {
    /// The commit this anchor was captured against, recorded on a
    /// best-effort basis: nothing keeps it reachable, so it may already be
    /// gone by the time the anchor is read back.
    pub genesis_rev: Oid,
    /// The repository-relative path of the anchored file at `genesis_rev`.
    pub path: String,
    /// The anchored byte span over the blob's stored bytes, always
    /// canonical (ARCHITECTURE.md, "Identity rule").
    pub span: Span,
}

impl IdentityNormalForm for AnchorIdentity {
    fn to_normal_form(&self) -> NormalForm {
        NormalForm::Struct(BTreeMap::from([
            (
                "genesis_rev".to_owned(),
                NormalForm::Hash(gix::ObjectId::from(self.genesis_rev)),
            ),
            ("path".to_owned(), NormalForm::Str(self.path.clone())),
            (
                "span".to_owned(),
                NormalForm::List(vec![
                    NormalForm::U64(self.span.start),
                    NormalForm::U64(self.span.end),
                ]),
            ),
        ]))
    }
}

/// Retained material (`anchor.retention`): derivable from [`AnchorIdentity`]
/// given the repository, never read by [`Anchor::id`]. Additive and
/// versioned — an algorithm or grammar upgrade changes what lands here, never
/// an anchor id.
#[derive(Debug, Clone, PartialEq, Eq, Facet, Default)]
pub struct AnchorHints {
    /// Fuzzy content signatures over the anchored span, for the
    /// [`crate::oracle::fingerprint`] oracle to match against once exact
    /// history tracing fails. [`crate::capture`] and
    /// [`crate::capture_worktree`] each emit exactly one,
    /// [`crate::fingerprint::MINHASH_SHINGLE_V1`]; an absent or stale
    /// fingerprint is recomputed on demand from `identity`, never trusted
    /// blindly.
    pub fingerprints: Vec<Fingerprint>,
    /// Grammar-aware structural descriptors (CST node kind, name path) a
    /// grammar-aware producer fills in. This crate has no grammar dependency,
    /// so [`crate::capture`] and [`crate::capture_worktree`] always emit an
    /// empty list here rather than invent one.
    pub descriptors: Vec<Descriptor>,
}

/// A grammar-aware structural descriptor: which grammar (by id and version),
/// which kind of node, at which name path — a function of blob and grammar,
/// filled in by a grammar-aware producer this crate does not implement.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct Descriptor {
    /// The grammar's identifier (e.g. a tree-sitter language name).
    pub grammar_id: String,
    /// The grammar's version.
    pub grammar_version: String,
    /// The CST node kind the anchored span sits at.
    pub node_kind: String,
    /// The path of names (module, item, field, ...) leading to that node.
    pub name_path: Vec<String>,
}

/// A durable pointer into source: authoritative at creation
/// (`anchor.immutable`) and never mutated — every function taking one
/// borrows it immutably, and every oracle in [`crate::oracle`] only ever
/// produces a [`crate::oracle::Candidate`], never a changed `Anchor`.
///
/// `identity` and `hints` are sibling subtrees: `identity` is
/// [`AnchorIdentity`], `hints` is [`AnchorHints`]. Serializing an `Anchor`
/// writes both as ordinary tree entries.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct Anchor {
    /// The non-derivable coordinates. [`Anchor::id`] hashes this alone.
    pub identity: AnchorIdentity,
    /// Retained, derivable, never identity-bearing.
    pub hints: AnchorHints,
}

impl Anchor {
    /// The anchor id (`anchor.identity`): the content hash of
    /// [`Anchor::identity`] alone, through the identity normal form,
    /// independent of [`Anchor::hints`].
    ///
    /// # Errors
    ///
    /// [`Error::NormalForm`] when the underlying `facet-git-tree` normal-form
    /// write fails.
    pub fn id(&self) -> Result<AnchorId> {
        crate::handle::hash_identity(&self.identity)
    }
}

/// Build the [`Anchor`] for `path` (and optionally `lines`) as it exists at
/// `revision` in `repo`, recording its byte span and a fresh capture-time
/// fingerprint (`anchor.retention`).
///
/// Fails when the path is not a file at that commit or the range does not
/// fit it (`anchor.definition`).
///
/// # Examples
///
/// ```
/// # let dir = tempfile::tempdir().expect("tempdir");
/// # std::process::Command::new("git").arg("init").arg("-q").arg(dir.path()).status().unwrap();
/// # std::fs::write(dir.path().join("file.txt"), "line 1\nline 2\nline 3\n").unwrap();
/// # std::process::Command::new("git").arg("-C").arg(dir.path()).args(["add", "-A"]).status().unwrap();
/// # std::process::Command::new("git").arg("-C").arg(dir.path())
/// #     .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "-m", "one"])
/// #     .status().unwrap();
/// let repo = gix::open(dir.path()).expect("open");
/// let anchor = gix_anchor::capture(&repo, "HEAD", "file.txt", None).expect("capture");
/// assert_eq!(anchor.identity.path, "file.txt");
/// ```
pub fn capture(
    repo: &gix::Repository,
    revision: &str,
    path: &str,
    lines: Option<LineRange>,
) -> Result<Anchor> {
    let commit = resolve_commit(repo, revision)?;
    let commit_id = commit.id().detach();
    let tree = commit
        .tree()
        .map_err(|error| Error::Object(error.to_string()))?;
    let entry = tree
        .lookup_entry_by_path(path)
        .map_err(|error| Error::Object(error.to_string()))?
        .filter(|entry| entry.mode().is_blob())
        .ok_or_else(|| Error::MissingPath {
            commit: commit_id,
            path: path.to_owned(),
        })?;
    let blob = entry.object_id();
    let content = read_blob(repo, blob)?;
    let span = span_for(&content, path, lines)?;

    Ok(Anchor {
        identity: AnchorIdentity {
            genesis_rev: commit_id.into(),
            path: path.to_owned(),
            span,
        },
        hints: hints_for(&content, span),
    })
}

/// Build the [`Anchor`] for `path` (and optionally `lines`) as it currently
/// sits in `repo`'s working tree (`anchor.working-tree`): the on-disk bytes
/// are written to the object database as a blob — the stored bytes a
/// [`Span`] is canonical over — so an anchor to uncommitted content survives
/// that content being committed, amended, or discarded. `identity.genesis_rev`
/// records `HEAD`'s commit, the same best-effort, never-load-bearing
/// coordinate a [`capture`]d anchor's is.
///
/// Fails with [`Error::NoWorkingTree`] on a bare repository, with
/// [`Error::MissingPath`] when `path` is not a readable file on disk, and
/// with [`Error::LinesOutOfRange`] when the range does not fit the on-disk
/// content.
pub fn capture_worktree(
    repo: &gix::Repository,
    path: &str,
    lines: Option<LineRange>,
) -> Result<Anchor> {
    let workdir = repo.workdir().ok_or(Error::NoWorkingTree)?;
    let commit_id = resolve_commit(repo, "HEAD")?.id().detach();
    let file = workdir.join(path);
    let missing = || Error::MissingPath {
        commit: commit_id,
        path: path.to_owned(),
    };
    if !file.is_file() {
        return Err(missing());
    }
    let content = std::fs::read(&file).map_err(|_source| missing())?;
    let span = span_for(&content, path, lines)?;
    // Written to the odb now (`anchor.working-tree`), so the blob exists
    // under its own id from the moment of capture, over the same bytes the
    // span is canonical over.
    repo.write_blob(content.as_slice())
        .map_err(|error| Error::Object(error.to_string()))?;

    Ok(Anchor {
        identity: AnchorIdentity {
            genesis_rev: commit_id.into(),
            path: path.to_owned(),
            span,
        },
        hints: hints_for(&content, span),
    })
}

/// Canonicalize `lines` against `content` into a [`Span`] — capture-time
/// canonicalization, legal per ARCHITECTURE.md's identity rule — or the
/// whole-blob span when `lines` is `None`.
fn span_for(content: &[u8], path: &str, lines: Option<LineRange>) -> Result<Span> {
    match lines {
        Some(range) => byte_span_of(content, path, range),
        None => Ok(Span::whole(content.len())),
    }
}

/// The capture-time [`AnchorHints`]: one [`Fingerprint`] over the anchored
/// span's bytes, no descriptors (this crate has no grammar dependency).
fn hints_for(content: &[u8], span: Span) -> AnchorHints {
    let bytes = span.slice(content).unwrap_or(content);
    AnchorHints {
        fingerprints: vec![capture_fingerprint(bytes)],
        descriptors: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "unit test; the panic is an assertion the type reflects as a struct at all"
    )]

    use facet::{Facet as _, Type, UserType};
    use gix::ObjectId;
    use rstest::rstest;

    use super::*;
    use crate::fixture::{commit_all, head, numbered, repo};

    fn range(start: u64, end: u64) -> Option<LineRange> {
        Some(LineRange { start, end })
    }

    #[test]
    fn capture_records_the_commit_and_a_byte_span() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();

        let anchor = capture(&git_repo, "HEAD", "file.txt", range(3, 4)).unwrap();
        assert_eq!(
            ObjectId::from(anchor.identity.genesis_rev).to_string(),
            head(dir.path())
        );
        assert_eq!(anchor.identity.path, "file.txt");
        let content = numbered(1..=10).into_bytes();
        assert_eq!(
            anchor.identity.span.slice(&content).unwrap(),
            b"line 3\nline 4\n"
        );
    }

    #[rstest]
    #[case::missing_path("absent.txt", None)]
    #[case::oversized_range("file.txt", range(2, 9))]
    fn capture_rejects_a_missing_path_and_an_oversized_range(
        #[case] path: &str,
        #[case] lines: Option<LineRange>,
    ) {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=3)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();

        let error = capture(&git_repo, "HEAD", path, lines).unwrap_err();
        assert!(matches!(
            error,
            Error::MissingPath { .. } | Error::LinesOutOfRange { .. }
        ));
    }

    #[test]
    fn capture_worktree_records_dirty_bytes_and_head() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let dirty = numbered(1..=10).replace("line 5\n", "line five\n");
        std::fs::write(dir.path().join("file.txt"), &dirty).unwrap();
        let git_repo = gix::open(dir.path()).unwrap();

        let anchor = capture_worktree(&git_repo, "file.txt", range(5, 6)).unwrap();
        assert_eq!(
            ObjectId::from(anchor.identity.genesis_rev).to_string(),
            head(dir.path())
        );
        assert_eq!(
            anchor.identity.span.slice(dirty.as_bytes()).unwrap(),
            b"line five\nline 6\n"
        );
    }

    #[rstest]
    #[case::missing_path("absent.txt", None)]
    #[case::oversized_range("file.txt", range(2, 9))]
    fn capture_worktree_rejects_a_missing_path_and_an_oversized_range(
        #[case] path: &str,
        #[case] lines: Option<LineRange>,
    ) {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=3)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();

        let error = capture_worktree(&git_repo, path, lines).unwrap_err();
        assert!(matches!(
            error,
            Error::MissingPath { .. } | Error::LinesOutOfRange { .. }
        ));
    }

    #[test]
    fn anchor_reflects_identity_then_hints() {
        let Type::User(UserType::Struct(struct_ty)) = Anchor::SHAPE.ty else {
            panic!("Anchor must reflect as a struct");
        };
        let names: Vec<_> = struct_ty.fields.iter().map(|f| f.name).collect();
        assert_eq!(names, vec!["identity", "hints"]);

        let Type::User(UserType::Struct(identity_ty)) = AnchorIdentity::SHAPE.ty else {
            panic!("AnchorIdentity must reflect as a struct");
        };
        let identity_names: Vec<_> = identity_ty.fields.iter().map(|f| f.name).collect();
        assert_eq!(identity_names, vec!["genesis_rev", "path", "span"]);

        let Type::User(UserType::Struct(hints_ty)) = AnchorHints::SHAPE.ty else {
            panic!("AnchorHints must reflect as a struct");
        };
        let hints_names: Vec<_> = hints_ty.fields.iter().map(|f| f.name).collect();
        assert_eq!(
            hints_names,
            vec!["fingerprints", "descriptors"],
            "identity must never hold a fingerprint or a descriptor"
        );
    }

    /// `anchor.identity`: the id is a pure function of `identity` alone, and
    /// a hint change — including a fingerprint algorithm upgrade — never
    /// changes it (ARCHITECTURE.md, "Identity rule").
    #[test]
    fn id_is_invariant_under_a_hints_change_and_varies_with_any_identity_coordinate() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(&git_repo, "HEAD", "file.txt", range(3, 4)).unwrap();

        let mut same_identity = anchor.clone();
        same_identity
            .hints
            .fingerprints
            .push(capture_fingerprint(b"unrelated"));
        same_identity.hints.descriptors.push(Descriptor {
            grammar_id: "rust".to_owned(),
            grammar_version: "1".to_owned(),
            node_kind: "fn_item".to_owned(),
            name_path: vec!["main".to_owned()],
        });
        assert_eq!(
            anchor.id().unwrap(),
            same_identity.id().unwrap(),
            "changing a hint must not change the id"
        );

        let mut different_path = anchor.clone();
        different_path.identity.path = "other.txt".to_owned();
        assert_ne!(anchor.id().unwrap(), different_path.id().unwrap());

        let mut different_span = anchor.clone();
        different_span.identity.span = Span { start: 0, end: 1 };
        assert_ne!(anchor.id().unwrap(), different_span.id().unwrap());

        let mut different_genesis = anchor.clone();
        different_genesis.identity.genesis_rev =
            gix::ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567")
                .unwrap()
                .into();
        assert_ne!(anchor.id().unwrap(), different_genesis.id().unwrap());
    }

    /// A line-range input canonicalizes to the same byte span regardless of
    /// a clean filter: `capture` always reads odb (post-clean-filter) bytes,
    /// so it is naturally clean-filter-stable — this pins the byte offsets a
    /// CRLF-normalizing filter would otherwise disagree about.
    #[test]
    fn line_range_canonicalizes_to_the_same_span_across_a_clean_filter() {
        let dir = repo();
        std::fs::write(dir.path().join(".gitattributes"), "file.txt text eol=lf\n").unwrap();
        std::fs::write(dir.path().join("file.txt"), "a\r\nb\r\nc\r\n").unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();

        let anchor = capture(&git_repo, "HEAD", "file.txt", range(2, 2)).unwrap();
        // The stored (clean-filtered) blob has LF line endings; the span
        // must be computed over those bytes, not the CRLF the filter
        // definition requested on checkout.
        assert_eq!(anchor.identity.span, Span { start: 2, end: 4 });
    }

    /// Byte-stability: a hard-coded anchor id for a fixed identity, so a
    /// future encoding change breaks loudly here rather than silently
    /// re-homing every anchor id in existence.
    #[test]
    fn anchor_id_is_byte_stable() {
        let identity = AnchorIdentity {
            genesis_rev: gix::ObjectId::from_hex(b"1111111111111111111111111111111111111111")
                .unwrap()
                .into(),
            path: "src/refdb.rs".to_owned(),
            span: Span {
                start: 4180,
                end: 4630,
            },
        };
        let id = crate::handle::hash_identity(&identity).unwrap();
        assert_eq!(
            id.to_string(),
            "8e50840508db65f7296d8cbedaee54a042efe948",
            "hard-coded against the frozen identity normal form mapping"
        );
    }
}
