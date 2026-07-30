//! Storage for [`crate::Comment`]: a genesis-keyed `gix-store` kind
//! (`comment`), one entity per comment instance — never per binding, since a
//! reply and the comment it replies to share a binding but must never share
//! an identity.
//!
//! [`Document`] is the wire shape; [`crate::comment::Comments`] hydrates its
//! own public [`crate::Comment`] straight from it, with no intermediate
//! translation type.

use facet::Facet;
use facet_git_tree::RawTree;
use gix::ObjectId;
use gix::objs::{Find, Write};
use gix_anchor::Binding;
use gix_store::{
    Committer, Entry, GixRefStore, Kind, Layout, RefPath, RefPrefix, RefSegment, RefStore, Typed,
};

use crate::comment::State;
use crate::error::{Error, Result};

/// The one kind every comment is an entity of.
const COMMENTS_KIND: &str = "comment";

/// The document committed at a comment's ref: `body`, the [`Binding`] it is
/// about — embedded inline, so the kind's published schema carries
/// `Binding`'s own shape rather than an opaque tree id — an optional
/// `attachment` tree, a `parent` link, a resolvable `state`, and
/// `created_at`.
///
/// `created_at`'s default lets a schema-generic writer (`git anchor add
/// comment`) omit it, since it has no value to supply. [`Store::create`] and
/// [`Store::update`] never rely on the default themselves: both always set a
/// real [`now_nanos`] before writing. The default only ever fires reading a
/// document *that* writer produced, and only on the typed path — see
/// [`now_nanos`]'s own doc for what it recovers there.
#[derive(Facet)]
pub(crate) struct Document {
    pub(crate) body: String,
    pub(crate) binding: Binding,
    /// Opaque passthrough; must already exist in the repo's object database,
    /// since a [`RawTree`] carries no content of its own.
    pub(crate) attachment: Option<RawTree>,
    /// An upstream comment's hex id, by convention.
    pub(crate) parent: Option<String>,
    pub(crate) state: Option<State>,
    /// Nanoseconds since the Unix epoch, set once at creation and forwarded
    /// unchanged by every later version — finer-grained than a commit's
    /// one-second author-time resolution.
    #[facet(default = now_nanos())]
    pub(crate) created_at: u64,
}

/// The current wall-clock time, in nanoseconds since the Unix epoch,
/// best-effort (`0` if the clock reads before the epoch).
///
/// Also [`Document::created_at`]'s `#[facet(default)]` source. That marker
/// only ever changes behavior for a document a *schema-generic* writer wrote
/// without a `created_at` entry at all: a typed read (`facet`'s own
/// `Partial::build`, independent of anything this crate does) then calls
/// this function fresh, at read time, to fill the unset field — recovering
/// "now" rather than `u64`'s own zero default, since no earlier value was
/// ever recorded to recover. It is not the comment's real creation time, and
/// it changes on every such read; it is the least-wrong value available for
/// data this crate itself never wrote incompletely. A dynamic (schema-only)
/// read of the same document instead leaves `created_at` absent from the
/// result entirely — no value, typed or not, to invent.
pub(crate) fn now_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// A comment's entity name under the `comment` kind:
/// `<target-hex>/<id-hex>`. Target first, so every comment about one target
/// is a single ref subtree — what [`Store::list`]'s `target` filter narrows
/// to via [`comment_group`].
///
/// `id` alone does not carry its target, so a lookup by id (`Store::find`)
/// only ever needs the identity half back — never the target half.
fn comment_group(target: ObjectId) -> RefPath {
    RefPath::from(hex(target))
}

/// The identity half of a `<target-hex>/<id-hex>` entity name, or `None`
/// when `path` is not that shape — the depth check is the slice pattern, not
/// a separate guard.
fn comment_id(path: &RefPath) -> Option<ObjectId> {
    let [target, id] = path.segments() else {
        return None;
    };
    oid(target)?;
    oid(id)
}

/// An [`ObjectId`]'s hex rendering is always a valid ref-name segment.
fn hex(id: ObjectId) -> RefSegment {
    RefSegment::new(id.to_string()).expect("object id hex is a valid ref segment")
}

fn oid(segment: &RefSegment) -> Option<ObjectId> {
    ObjectId::from_hex(segment.as_str().as_bytes()).ok()
}

fn segment(value: &str) -> RefSegment {
    RefSegment::new(value).expect("built-in ref segment is valid")
}

/// Comments at `<prefix>/data/comment/<target-hex>/<id-hex>`, the kind's
/// schema at `<prefix>/schema/comment` — disjoint subtrees, so no kind can
/// ever collide with the schema namespace and a data walk never filters a
/// schema out.
fn layout(prefix: &RefPrefix) -> Layout {
    Layout {
        data: prefix.child(&segment("data")),
        schema: prefix.child(&segment("schema")),
    }
}

/// A store of [`Document`]s over a [`RefStore`]/[`Committer`] and an object
/// database: one comment per genesis identity, editable with full history.
pub(crate) struct Store<R, O> {
    inner: gix_store::Store<R, O>,
}

impl<R, O> Store<R, O>
where
    R: RefStore + Committer,
    O: Find + Write,
{
    pub(crate) fn new(refs: R, objects: O, prefix: &RefPrefix) -> Self {
        Store {
            inner: gix_store::Store::with_layout(refs, objects, layout(prefix)),
        }
    }

    /// Create a genesis-keyed comment: a fresh identity — the oid of the
    /// parentless commit this method mints, never derived from `binding` —
    /// so calling this twice on the same binding creates two distinct
    /// comments. [`Store::update`] edits one of them by id afterward.
    ///
    /// Returns the new comment's identity oid.
    ///
    /// # Errors
    ///
    /// Propagates a `Document` serialization failure, and any underlying
    /// ref-store or object-database failure.
    pub(crate) fn create(
        &self,
        binding: &Binding,
        body: &str,
        attachment: Option<ObjectId>,
        parent: Option<String>,
        state: Option<State>,
        message: &str,
    ) -> Result<ObjectId> {
        let target = binding.target();
        let document = Document {
            body: body.to_owned(),
            binding: binding.clone(),
            attachment: attachment.map(RawTree::new),
            parent,
            state,
            created_at: now_nanos(),
        };
        let comments = self.published()?;
        Ok(comments
            .write(&document)
            .message(message)
            .anonymous_under(&comment_group(target))?)
    }

    /// Commit a new version of the comment `id` forward onto its own ref:
    /// same identity, fresh `body`/`attachment`/`parent`/`state`, full
    /// history preserved. The binding and `created_at` are carried forward
    /// unchanged.
    ///
    /// Returns `id` unchanged.
    ///
    /// # Errors
    ///
    /// [`Error::Resolve`] when no comment with `id` exists. Otherwise
    /// propagates a `Document` serialization failure or any underlying
    /// ref-store or object-database failure.
    pub(crate) fn update(
        &self,
        id: ObjectId,
        body: &str,
        attachment: Option<ObjectId>,
        parent: Option<String>,
        state: Option<State>,
        message: &str,
    ) -> Result<ObjectId> {
        let Some(name) = self.find(id)? else {
            return Err(Error::Resolve(id.to_string()));
        };
        let missing = || Error::Resolve(id.to_string());

        self.published()?.try_update::<Error>(&name, |current| {
            // `binding` and `created_at` come off whatever version this
            // commits over, so a concurrent write is carried forward rather
            // than clobbered — and a concurrent *delete* is refused.
            let current = current.ok_or_else(missing)?;
            Ok((
                message.to_owned(),
                Document {
                    body: body.to_owned(),
                    binding: current.value.binding.clone(),
                    attachment: attachment.map(RawTree::new),
                    parent: parent.clone(),
                    state,
                    created_at: current.value.created_at,
                },
            ))
        })?;
        Ok(id)
    }

    /// Every stored comment, or only those about `target` when given,
    /// paired with its identity oid, sorted by id.
    ///
    /// # Errors
    ///
    /// Propagates a ref, commit, or tree-read failure.
    pub(crate) fn list(
        &self,
        target: Option<ObjectId>,
    ) -> Result<Vec<(ObjectId, Entry<Document>)>> {
        let comments = self.comments();
        let names = match target {
            Some(target) => comments.list_under(&comment_group(target))?,
            None => comments.list()?,
        };
        let mut out = Vec::new();
        for name in names {
            let Some(id) = comment_id(&name) else {
                continue;
            };
            if let Some(entry) = comments.get_entry(&name)? {
                out.push((id, entry));
            }
        }
        out.sort_by_key(|(id, _)| *id);
        Ok(out)
    }

    /// A single comment's entry by its identity oid. `None` when no comment
    /// with that id exists. Accepts only a full oid — no prefix resolution.
    ///
    /// # Errors
    ///
    /// Propagates a ref, commit, or tree-read failure.
    pub(crate) fn get(&self, id: ObjectId) -> Result<Option<Entry<Document>>> {
        let Some(name) = self.find(id)? else {
            return Ok(None);
        };
        Ok(self.comments().get_entry(&name)?)
    }

    /// The document committed directly at `commit`, rather than at a ref's
    /// current tip — the version-history counterpart to [`Store::get`], for
    /// reading an older entry off [`Store::history`]'s list.
    ///
    /// # Errors
    ///
    /// Propagates a commit- or tree-read failure. Does not check that
    /// `commit` is actually reachable from any comment ref.
    pub(crate) fn get_at(&self, commit: ObjectId) -> Result<Entry<Document>> {
        Ok(self.comments().get_entry_at(commit)?)
    }

    /// Delete a comment's ref. Returns whether it existed.
    ///
    /// # Errors
    ///
    /// Propagates a ref-lookup or deletion failure.
    pub(crate) fn remove(&self, id: ObjectId) -> Result<bool> {
        let Some(name) = self.find(id)? else {
            return Ok(false);
        };
        Ok(self.comments().remove(&name)?)
    }

    /// The version history (commit ids, tip-first) of a comment. Empty if
    /// absent.
    ///
    /// # Errors
    ///
    /// Propagates a ref or commit-read failure.
    pub(crate) fn history(&self, id: ObjectId) -> Result<Vec<ObjectId>> {
        match self.find(id)? {
            Some(name) => Ok(self.comments().history(&name)?),
            None => Ok(Vec::new()),
        }
    }

    // ── internals ────────────────────────────────────────────────────────

    fn comments(&self) -> Kind<'_, Typed<Document>, R, O> {
        self.inner.kind(segment(COMMENTS_KIND))
    }

    /// [`Self::comments`] with its schema published, for the write paths.
    ///
    /// Publishing commits forward unconditionally, so this checks first —
    /// otherwise every open would mint a fresh, identical schema commit.
    fn published(&self) -> Result<Kind<'_, Typed<Document>, R, O>> {
        let comments = self.comments();
        if comments.schema().get()?.is_none() {
            comments.publish()?;
        }
        Ok(comments)
    }

    /// The entity name of the comment with identity `id`, or `None` when
    /// there is none.
    ///
    /// `id` alone does not carry its target, so this scans the kind's ref
    /// names — never its objects — for the one whose second segment matches.
    fn find(&self, id: ObjectId) -> Result<Option<RefPath>> {
        Ok(self
            .comments()
            .list()?
            .into_iter()
            .find(|name| comment_id(name) == Some(id)))
    }
}

/// A [`Store`] over a `gix` repository's own refs and object database.
pub(crate) type RepoStore<'r> = Store<GixRefStore<'r>, &'r gix::OdbHandle>;

impl<'r> RepoStore<'r> {
    /// Open a store over `repo` rooted at `prefix`.
    pub(crate) fn with_prefix(repo: &'r gix::Repository, prefix: RefPrefix) -> Self {
        Store::new(GixRefStore::new(repo), &repo.objects, &prefix)
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

    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::convert::Infallible;

    use facet_git_tree::{Node, ObjectStore, Schema, schema_of};
    use gix::actor::Signature;
    use gix::objs::FindExt;
    use gix_store::{ApplyError, MemoryRefStore, RefEdit, RefName};

    use super::*;

    const PREFIX: &str = "refs/comments";

    fn oid_of(byte: u8) -> ObjectId {
        let digit = format!("{byte:x}");
        ObjectId::from_hex(digit.repeat(40).as_bytes()).expect("valid hex")
    }

    fn prefix() -> RefPrefix {
        RefPrefix::new(PREFIX).expect("PREFIX is a valid ref prefix")
    }

    fn memory_store() -> Store<MemoryRefStore, ObjectStore> {
        Store::new(MemoryRefStore::new(), ObjectStore::default(), &prefix())
    }

    // ── layout ───────────────────────────────────────────────────────────

    #[test]
    fn create_writes_the_ref_at_prefix_target_id() {
        let store = memory_store();
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let id = store
            .create(&binding, "note", None, None, None, "msg")
            .unwrap();

        let expected = RefName::new(format!(
            "{PREFIX}/data/{COMMENTS_KIND}/{}/{id}",
            binding.target()
        ))
        .unwrap();
        assert!(store.inner.refs().read(&expected).unwrap().is_some());
    }

    #[test]
    fn prefix_boundary_is_a_whole_segment_not_a_string_prefix() {
        let store = memory_store();
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let id = store
            .create(&binding, "note", None, None, None, "msg")
            .unwrap();

        // "refs/commentsfoo" shares a string prefix with "refs/comments" but
        // is not a `/`-bounded child of it.
        let foreign = RefName::new(format!(
            "refs/commentsfoo/{}/{}",
            binding.target(),
            oid_of(2)
        ))
        .unwrap();
        store
            .inner
            .refs()
            .apply(RefEdit::Create {
                name: foreign.clone(),
                new: oid_of(3),
            })
            .unwrap();

        // The foreign ref really is there in the backing store...
        assert!(store.inner.refs().read(&foreign).unwrap().is_some());
        // ...but only what lives under this store's own prefix is a comment.
        let comments = store.list(None).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].0, id);
    }

    /// Comments are one kind whatever they are about, so the schema is
    /// published once for the whole store — not once per target.
    #[test]
    fn every_target_shares_one_published_schema() {
        let store = memory_store();
        for byte in 1..=4 {
            let binding = Binding::Commit {
                commit: oid_of(byte).into(),
            };
            store
                .create(&binding, "note", None, None, None, "msg")
                .unwrap();
        }

        let schemas = store
            .inner
            .refs()
            .prefixed(&prefix().child(&segment("schema")))
            .unwrap();
        assert_eq!(
            schemas.len(),
            1,
            "four targets, one schema ref: {schemas:?}"
        );
        assert_eq!(store.inner.kinds().unwrap(), vec![segment(COMMENTS_KIND)]);
    }

    /// The identity scheme may never truncate its name: a shortened segment
    /// would put the comment namespace's collision odds below the object
    /// database's own.
    #[test]
    fn the_identity_scheme_names_entities_by_full_oids() {
        let store = memory_store();
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let id = store
            .create(&binding, "note", None, None, None, "msg")
            .unwrap();

        let name = store.find(id).unwrap().expect("comment exists");
        let [target, leaf] = name.segments() else {
            panic!("entity name is a <target>/<id> pair: {name}");
        };
        assert_eq!(target.as_str(), binding.target().to_string());
        assert_eq!(leaf.as_str(), id.to_string());
        assert_eq!(leaf.as_str().len(), 40, "full hex, not truncated");
    }

    // ── round trip ───────────────────────────────────────────────────────

    #[test]
    fn create_then_get_round_trips_with_no_repository() {
        let store = memory_store();
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let id = store
            .create(&binding, "hello", None, None, None, "msg")
            .unwrap();

        let entry = store.get(id).unwrap().expect("comment exists");
        assert_eq!(entry.value.body, "hello");
        assert_eq!(entry.value.binding, binding);

        let listed = store.list(None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0, id);
    }

    #[test]
    fn update_versions_the_same_ref_forward() {
        let store = memory_store();
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let id = store
            .create(&binding, "v1", None, None, None, "msg")
            .unwrap();
        store.update(id, "v2", None, None, None, "edit").unwrap();
        assert_eq!(store.history(id).unwrap().len(), 2);
        assert_eq!(store.get(id).unwrap().unwrap().value.body, "v2");
    }

    #[test]
    fn create_twice_on_the_same_binding_mints_two_distinct_refs() {
        let store = memory_store();
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let first = store.create(&binding, "a", None, None, None, "a").unwrap();
        let second = store.create(&binding, "b", None, None, None, "b").unwrap();
        assert_ne!(first, second, "genesis-keyed: distinct identities");
        assert_eq!(store.list(Some(binding.target())).unwrap().len(), 2);
    }

    // ── created_at forwarding ────────────────────────────────────────────

    #[test]
    fn update_preserves_the_original_created_at() {
        let store = memory_store();
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let id = store.create(&binding, "a", None, None, None, "a").unwrap();
        let first_created_at = store.get(id).unwrap().unwrap().value.created_at;

        std::thread::sleep(std::time::Duration::from_millis(2));
        store.update(id, "b", None, None, None, "b").unwrap();

        let second_created_at = store.get(id).unwrap().unwrap().value.created_at;
        assert_eq!(second_created_at, first_created_at);
    }

    // ── history ──────────────────────────────────────────────────────────

    #[test]
    fn history_lists_commits_tip_first() {
        let store = memory_store();
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let id = store
            .create(&binding, "v1", None, None, None, "msg")
            .unwrap();
        let first_commit = store.get(id).unwrap().unwrap().commit;
        store.update(id, "v2", None, None, None, "edit").unwrap();
        let second_commit = store.get(id).unwrap().unwrap().commit;
        assert_ne!(first_commit, second_commit);

        assert_eq!(
            store.history(id).unwrap(),
            vec![second_commit, first_commit]
        );
    }

    // ── fault-injecting RefStore: CAS retry paths ───────────────────────

    /// A scripted failure for [`FlakyRefStore::apply`].
    enum Injection {
        /// Land this edit for real first, so the caller's own edit then
        /// fails against a genuine precondition mismatch on the backend —
        /// simulates a concurrent writer that actually won the race.
        Concurrent(RefEdit),
        /// Fail immediately with nothing written — contention indistinguishable
        /// from a real race by the caller, but the backend never changes.
        Phantom,
    }

    /// Wraps a [`MemoryRefStore`], scripting its first `apply` calls to fail
    /// — exercises [`Store`]'s `LostRace` retry paths without a real
    /// concurrent writer.
    struct FlakyRefStore {
        inner: MemoryRefStore,
        injections: RefCell<VecDeque<Injection>>,
    }

    impl FlakyRefStore {
        fn new() -> Self {
            Self {
                inner: MemoryRefStore::new(),
                injections: RefCell::new(VecDeque::new()),
            }
        }

        fn push_concurrent(&self, edit: RefEdit) {
            self.injections
                .borrow_mut()
                .push_back(Injection::Concurrent(edit));
        }

        fn push_phantom(&self) {
            self.injections.borrow_mut().push_back(Injection::Phantom);
        }
    }

    impl RefStore for FlakyRefStore {
        type Error = Infallible;

        fn read(&self, name: &RefName) -> std::result::Result<Option<ObjectId>, Self::Error> {
            self.inner.read(name)
        }

        fn prefixed(
            &self,
            prefix: &RefPrefix,
        ) -> std::result::Result<Vec<(RefName, ObjectId)>, Self::Error> {
            self.inner.prefixed(prefix)
        }

        fn apply(&self, edit: RefEdit) -> std::result::Result<(), ApplyError<Self::Error>> {
            match self.injections.borrow_mut().pop_front() {
                Some(Injection::Concurrent(winner)) => {
                    self.inner
                        .apply(winner)
                        .expect("injected concurrent edit applies cleanly");
                    self.inner.apply(edit)
                }
                Some(Injection::Phantom) => Err(ApplyError::LostRace {
                    name: edit.name().clone(),
                    expected: edit.expectation(),
                }),
                None => self.inner.apply(edit),
            }
        }
    }

    impl Committer for FlakyRefStore {
        type Error = Infallible;

        fn signature(&self) -> std::result::Result<Signature, Self::Error> {
            self.inner.signature()
        }
    }

    fn flaky_store() -> Store<FlakyRefStore, ObjectStore> {
        Store::new(FlakyRefStore::new(), ObjectStore::default(), &prefix())
    }

    /// The ref a comment's entity lives at.
    fn comment_ref<R, O>(store: &Store<R, O>, target: ObjectId, id: ObjectId) -> RefName
    where
        R: RefStore + Committer,
        O: Find + Write,
    {
        store
            .comments()
            .reference(&comment_group(target).join(&hex(id)))
    }

    /// A backend whose author and committer identities differ, as a
    /// repository configuring `author.*` apart from `committer.*` does.
    struct SplitIdentity(MemoryRefStore);

    impl RefStore for SplitIdentity {
        type Error = Infallible;

        fn read(&self, name: &RefName) -> std::result::Result<Option<ObjectId>, Self::Error> {
            self.0.read(name)
        }

        fn prefixed(
            &self,
            prefix: &RefPrefix,
        ) -> std::result::Result<Vec<(RefName, ObjectId)>, Self::Error> {
            self.0.prefixed(prefix)
        }

        fn apply(&self, edit: RefEdit) -> std::result::Result<(), ApplyError<Self::Error>> {
            self.0.apply(edit)
        }
    }

    impl Committer for SplitIdentity {
        type Error = Infallible;

        fn signature(&self) -> std::result::Result<Signature, Self::Error> {
            let mut signature = self.0.signature()?;
            signature.name = "Committer".into();
            Ok(signature)
        }

        fn author(&self) -> std::result::Result<Signature, Self::Error> {
            let mut signature = self.0.signature()?;
            signature.name = "Author".into();
            Ok(signature)
        }
    }

    /// The author and committer names recorded on `commit`.
    fn identities(objects: &ObjectStore, commit: ObjectId) -> (String, String) {
        let mut buf = Vec::new();
        let data = objects.find(&commit, &mut buf).expect("commit present");
        let commit =
            gix::objs::CommitRef::from_bytes(data.data, data.object_hash).expect("valid commit");
        (
            commit.author().unwrap().name.to_string(),
            commit.committer().unwrap().name.to_string(),
        )
    }

    #[test]
    fn a_comments_author_comes_from_the_author_identity_not_the_committer() {
        let store = Store::new(
            SplitIdentity(MemoryRefStore::new()),
            ObjectStore::default(),
            &prefix(),
        );
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let id = store
            .create(&binding, "note", None, None, None, "msg")
            .unwrap();
        let commit = store.history(id).unwrap()[0];

        let (author, committer) = identities(store.inner.objects(), commit);
        assert_eq!(author, "Author");
        assert_eq!(committer, "Committer");
    }

    /// Commit a comment the way a concurrent writer would — a real,
    /// schema-bound entity commit parented on `parent` — without going
    /// through the ref under test, so it can be scripted as a
    /// [`FlakyRefStore::push_concurrent`] winner.
    ///
    /// The scratch name is not a `<target-hex>/<id-hex>` pair, so
    /// [`Store::list`] and [`Store::find`] never see it.
    fn write_comment_commit(
        store: &Store<FlakyRefStore, ObjectStore>,
        binding: &Binding,
        body: &str,
        created_at: u64,
        parent: Option<ObjectId>,
    ) -> ObjectId {
        let scratch = RefPath::new("scratch/winner").expect("valid entity path");
        let comments = store.published().expect("publish the comment schema");
        if let Some(parent) = parent {
            store
                .inner
                .refs()
                .apply(RefEdit::Create {
                    name: comments.reference(&scratch),
                    new: parent,
                })
                .expect("scratch ref");
        }
        let document = Document {
            body: body.to_owned(),
            binding: binding.clone(),
            attachment: None,
            parent: None,
            state: None,
            created_at,
        };
        comments
            .write(&document)
            .message("concurrent")
            .at(&scratch)
            .expect("write concurrent commit")
    }

    #[test]
    fn update_retries_and_carries_the_winners_binding_and_created_at_forward() {
        let store = flaky_store();
        let original_binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let id = store
            .create(&original_binding, "a", None, None, None, "a")
            .unwrap();
        let refname = comment_ref(&store, original_binding.target(), id);
        let original_tip = store.inner.refs().read(&refname).unwrap().unwrap();

        // A concurrent writer's version carries a different binding and a
        // distinguishable created_at: `update` must pick both up off the
        // winning tip, not off the value read before the race.
        let winner_binding = Binding::Commit {
            commit: oid_of(2).into(),
        };
        let winner_created_at = 123_456_789;
        let winner_commit = write_comment_commit(
            &store,
            &winner_binding,
            "concurrent",
            winner_created_at,
            Some(original_tip),
        );
        store.inner.refs().push_concurrent(RefEdit::Update {
            name: refname.clone(),
            expected: original_tip,
            new: winner_commit,
        });

        store
            .update(id, "edited", None, None, None, "edit")
            .unwrap();

        let after = store.get(id).unwrap().unwrap();
        assert_eq!(after.value.body, "edited");
        assert_eq!(
            after.value.binding, winner_binding,
            "carries the race winner's binding forward"
        );
        assert_eq!(after.value.created_at, winner_created_at);
    }

    #[test]
    fn remove_retries_and_deletes_the_winning_tip() {
        let store = flaky_store();
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let id = store
            .create(&binding, "v1", None, None, None, "msg")
            .unwrap();
        let refname = comment_ref(&store, binding.target(), id);
        let original_tip = store.inner.refs().read(&refname).unwrap().unwrap();

        let winner_commit =
            write_comment_commit(&store, &binding, "concurrent", 42, Some(original_tip));
        store.inner.refs().push_concurrent(RefEdit::Update {
            name: refname.clone(),
            expected: original_tip,
            new: winner_commit,
        });

        assert!(store.remove(id).unwrap());
        assert!(store.get(id).unwrap().is_none());
        assert!(store.inner.refs().read(&refname).unwrap().is_none());
    }

    #[test]
    fn create_retries_to_success_on_pure_contention() {
        let store = flaky_store();
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        store.inner.refs().push_phantom();

        let id = store
            .create(&binding, "body", None, None, None, "msg")
            .unwrap();
        assert_eq!(store.get(id).unwrap().unwrap().value.body, "body");
    }

    // ── reflection: schema embeds Binding's shape ───────────────────────

    /// `DEVPLAN-boundary.md`'s "Locating the binding field by reflection":
    /// resolve `schema`'s root through one [`Node::Ref`] indirection into
    /// `defs` to a [`Node::Struct`], then check whether any field, itself
    /// resolved through the same `defs`, is structurally equal to
    /// [`schema_of::<Binding>`]'s own root definition. A hand-written shape
    /// walker would defeat the point — this is `==` on ordinary `Node`
    /// values.
    fn is_anchorable(schema: &Schema) -> bool {
        let canonical = schema_of::<Binding>().expect("Binding has a schema");
        let canonical_root = resolve(&canonical, &canonical.root).expect("Binding's root resolves");

        let Some(Node::Struct(fields)) = resolve(schema, &schema.root) else {
            return false;
        };
        fields
            .values()
            .any(|field| resolve(schema, &field.node) == Some(canonical_root))
    }

    /// One [`Node::Ref`] indirection into `schema.defs`, or the node itself
    /// when it is not a `Ref`.
    fn resolve<'s>(schema: &'s Schema, node: &'s Node) -> Option<&'s Node> {
        match node {
            Node::Ref(name) => schema.defs.get(name),
            other => Some(other),
        }
    }

    /// A kind with no [`Binding`] field is not anchorable — the negative
    /// case [`is_anchorable`] must reject.
    #[derive(facet::Facet)]
    struct NotAnchorable {
        text: String,
    }

    #[test]
    fn published_schema_embeds_bindings_shape_by_reflection() {
        let store = memory_store();
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        store
            .create(&binding, "note", None, None, None, "msg")
            .unwrap();

        let schema = store
            .comments()
            .schema()
            .get()
            .unwrap()
            .expect("schema published by create");

        assert!(
            is_anchorable(&schema),
            "comment's published schema must embed Binding's shape"
        );

        let canonical = schema_of::<Binding>().unwrap();
        assert_eq!(
            schema.defs.get("Binding"),
            canonical.defs.get("Binding"),
            "concrete form of the same check: the def tables agree exactly"
        );

        let unrelated = schema_of::<NotAnchorable>().unwrap();
        assert!(
            !is_anchorable(&unrelated),
            "a schema with no Binding field must not be reported anchorable"
        );
    }
}
