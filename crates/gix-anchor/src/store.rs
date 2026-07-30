//! [`Store`]: notes — arbitrary content attached to a [`Binding`]'s target —
//! persisted as `gix-store` entities of one kind, `notes`.
//!
//! Two identity schemes, chosen by write method:
//! - **Binding-keyed** ([`Store::attach`]/[`Store::attach_with_attachment`]):
//!   one note per (target, binding-identity), keyed by the binding's own
//!   serialized tree oid. Re-attaching a binding commits a new version
//!   forward onto the same entity.
//! - **Genesis-keyed** ([`Store::create`]/[`Store::update`]): one note per
//!   *instance*, keyed by the oid of the commit [`Store::create`] mints —
//!   never the binding's — so two notes about the same binding (a reply and
//!   what it replies to) get distinct identities. [`Store::update`] versions
//!   one forward by id.
//!
//! Every note also carries `parent` and `state`, opaque to this crate
//! (`None` for the binding-keyed scheme) — a downstream consumer like
//! `gix-comment` builds reply threads and a resolvable lifecycle on them.

use facet::Facet;
use facet_git_tree::RawTree;
use gix::ObjectId;
use gix::objs::{Find, Write};
use gix_store::{
    Committer, Entry, GixRefStore, Kind, Layout, RefPath, RefPrefix, RefSegment, RefStore, Typed,
};

use crate::binding::Binding;
use crate::error::{Error, Result};

/// [`Store::open`]'s default prefix.
const ANCHOR_PREFIX: &str = "refs/anchors";

/// The one kind every note is an entity of, whatever its target, so a single
/// published schema covers the whole store.
const NOTES_KIND: &str = "notes";

/// The document committed at a note's ref: `body`, the [`Binding`] it is
/// attached to — embedded inline, so a kind's published schema carries
/// `Binding`'s own shape rather than an opaque tree id — an optional
/// `attachment` tree embedded by tree id, and opaque `parent`/`state`
/// bookkeeping. Content either the binding or the attachment references
/// stays reachable through the note's own tree (`anchor.retention`).
#[derive(Facet)]
struct Note {
    body: Vec<u8>,
    binding: Binding,
    /// Opaque passthrough; must already exist in the repo's object database,
    /// since a [`RawTree`] carries no content of its own.
    attachment: Option<RawTree>,
    /// Opaque passthrough (an upstream note's hex id, by convention).
    parent: Option<String>,
    /// Opaque passthrough (a free-form lifecycle tag).
    state: Option<String>,
    /// Nanoseconds since the Unix epoch, set once at creation and forwarded
    /// unchanged by every later version — finer-grained than a commit's
    /// one-second author-time resolution.
    created_at: u64,
}

/// The current wall-clock time, in nanoseconds since the Unix epoch,
/// best-effort (`0` if the clock reads before the epoch).
fn now_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// A note's entity name under the `notes` kind: `<target-hex>/<id-hex>`.
///
/// Target first, so every note about one target is a single ref subtree —
/// what [`Store::list`] narrows to with one `target`.
struct NoteName {
    target: ObjectId,
    id: ObjectId,
}

impl NoteName {
    fn path(&self) -> RefPath {
        Self::group(self.target).join(&hex(self.id))
    }

    /// The single-segment path every note about `target` nests under.
    fn group(target: ObjectId) -> RefPath {
        RefPath::from(hex(target))
    }

    /// `None` when `path` is not a `<target-hex>/<id-hex>` pair — the depth
    /// check is the slice pattern, not a separate guard.
    fn parse(path: &RefPath) -> Option<Self> {
        let [target, id] = path.segments() else {
            return None;
        };
        Some(NoteName {
            target: oid(target)?,
            id: oid(id)?,
        })
    }
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

/// Notes at `<prefix>/data/notes/<target-hex>/<id-hex>`, the kind's schema at
/// `<prefix>/schema/notes` — disjoint subtrees, so no kind can ever collide
/// with the schema namespace and a data walk never filters a schema out.
fn layout(prefix: &RefPrefix) -> Layout {
    Layout {
        data: prefix.child(&segment("data")),
        schema: prefix.child(&segment("schema")),
    }
}

/// A store of notes over a [`RefStore`]/[`Committer`] and an object database:
/// one note per identity (binding- or genesis-keyed, depending on which write
/// method is used), editable with full history.
pub struct Store<R, O> {
    inner: gix_store::Store<R, O>,
}

/// A note read back from the [`Store`].
pub struct StoredNote {
    /// The note's identity oid: the binding's serialized tree oid
    /// (binding-keyed), or the genesis commit oid (genesis-keyed).
    pub id: ObjectId,
    /// [`Binding::target`] — the ref-path grouping key.
    pub target: ObjectId,
    /// The binding this note is attached to, recovered from storage.
    pub binding: Binding,
    /// The arbitrary content attached to `binding`.
    pub body: Vec<u8>,
    /// The latest version's commit summary (first line of the message).
    pub message: String,
    /// The optional attachment tree's oid, as handed to
    /// [`Store::attach_with_attachment`]/[`Store::create`]/[`Store::update`].
    pub attachment: Option<ObjectId>,
    /// Opaque upstream link — conventionally an upstream note's hex id.
    /// `None` for a binding-keyed note or one with no parent.
    pub parent: Option<String>,
    /// Opaque lifecycle tag, set by a caller such as `gix-comment`.
    pub state: Option<String>,
    /// The commit this note was read from — a note ref's tip for
    /// [`Store::get`]/[`Store::list`], or the requested commit for
    /// [`Store::get_at`]. Its author and time are the note's own.
    pub commit: ObjectId,
    /// [`Note::created_at`], forwarded unchanged across versions. A
    /// finer-grained ordering tiebreak than `commit`'s author time, not a
    /// substitute for it.
    pub created_at: u64,
}

impl<R, O> Store<R, O>
where
    R: RefStore + Committer,
    O: Find + Write,
{
    /// Open a store over `refs` and `objects`, rooted at `prefix`.
    pub fn new(refs: R, objects: O, prefix: &RefPrefix) -> Self {
        Store {
            inner: gix_store::Store::with_layout(refs, objects, layout(prefix)),
        }
    }

    /// Attach `body` to the object `binding` names, git-notes style: one
    /// note per (target, binding-identity), editable with history.
    /// `message` sets the commit summary, defaulting to `anchor <target>`.
    ///
    /// Returns the note's identity oid.
    ///
    /// # Errors
    ///
    /// Propagates a [`Binding::serialize_into`] or `Note` serialization
    /// failure, and any underlying ref-store or object-database failure
    /// ([`Error::Git`]).
    pub fn attach(
        &self,
        binding: &Binding,
        body: &[u8],
        message: Option<&str>,
    ) -> Result<ObjectId> {
        self.attach_with_attachment(binding, body, None, message)
    }

    /// [`Store::attach`], plus an `attachment` tree embedded verbatim in the
    /// note document (`anchor.retention`) — it stays reachable through the
    /// note's own ref, the same way the binding's own blobs do.
    ///
    /// `attachment` must already exist in the repository's object database.
    /// It is opaque here: this crate never decodes it. Returns the note's
    /// identity oid, exactly as [`Store::attach`] does. `parent`/`state` are
    /// always written `None`; a caller that needs them wants
    /// [`Store::create`] instead.
    ///
    /// # Errors
    ///
    /// The same failures as [`Store::attach`].
    pub fn attach_with_attachment(
        &self,
        binding: &Binding,
        body: &[u8],
        attachment: Option<ObjectId>,
        message: Option<&str>,
    ) -> Result<ObjectId> {
        let target = binding.target();
        let id = binding.serialize_into(self.inner.objects())?;
        let default_summary = format!("anchor {target}");
        let summary = message.unwrap_or(&default_summary);

        self.published()?
            .update(&NoteName { target, id }.path(), |current| {
                (
                    summary.to_owned(),
                    Note {
                        body: body.to_vec(),
                        binding: binding.clone(),
                        attachment: attachment.map(RawTree::new),
                        parent: None,
                        state: None,
                        // A re-attach keeps the note's place in a caller's
                        // creation-order tiebreak, so it forwards whatever
                        // version it commits over rather than restamping.
                        created_at: current.map_or_else(now_nanos, |entry| entry.value.created_at),
                    },
                )
            })?;
        Ok(id)
    }

    /// Create a genesis-keyed note: `body` (plus an optional `attachment`,
    /// `parent`, and `state`, all opaque to this crate) attached to whatever
    /// `binding` names, but under a *fresh* identity — the oid of the
    /// parentless commit this method mints, never derived from `binding` —
    /// so calling this twice on the same binding creates two distinct notes
    /// rather than versioning one forward. [`Store::update`] edits one of
    /// them by id afterward.
    ///
    /// `message` sets the commit summary; unlike [`Store::attach`], there is
    /// no default.
    ///
    /// Returns the new note's identity oid (the genesis commit's own oid).
    ///
    /// # Errors
    ///
    /// Propagates a `Note` serialization failure, and any underlying
    /// ref-store or object-database failure ([`Error::Git`]).
    pub fn create(
        &self,
        binding: &Binding,
        body: &[u8],
        attachment: Option<ObjectId>,
        parent: Option<String>,
        state: Option<String>,
        message: &str,
    ) -> Result<ObjectId> {
        let target = binding.target();
        let note = Note {
            body: body.to_vec(),
            binding: binding.clone(),
            attachment: attachment.map(RawTree::new),
            parent,
            state,
            created_at: now_nanos(),
        };
        // The identity is the minted commit's own oid — unpredictable ahead of
        // time, unlike the binding-keyed scheme's deterministic id.
        let notes = self.published()?;
        Ok(notes
            .write(&note)
            .message(message)
            .anonymous_under(&NoteName::group(target))?)
    }

    /// Commit a new version of the genesis-keyed note `id` forward onto its
    /// own ref — the genesis-keyed counterpart to re-[`Store::attach`]ing:
    /// same identity, fresh `body`/`attachment`/`parent`/`state`, full
    /// history preserved. The binding is carried forward unchanged.
    ///
    /// Returns `id` unchanged.
    ///
    /// # Errors
    ///
    /// [`Error::Resolve`] when no note with `id` exists. Otherwise
    /// propagates a `Note` serialization failure or any underlying ref-store
    /// or object-database failure ([`Error::Git`]).
    pub fn update(
        &self,
        id: ObjectId,
        body: &[u8],
        attachment: Option<ObjectId>,
        parent: Option<String>,
        state: Option<String>,
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
                Note {
                    body: body.to_vec(),
                    binding: current.value.binding.clone(),
                    attachment: attachment.map(RawTree::new),
                    parent: parent.clone(),
                    state: state.clone(),
                    created_at: current.value.created_at,
                },
            ))
        })?;
        Ok(id)
    }

    /// Every stored note, or only those attached to `target` when given,
    /// sorted by id.
    ///
    /// # Errors
    ///
    /// Propagates a ref, commit, or tree-read failure, and a malformed or
    /// unrecognized stored [`Binding`] shape.
    pub fn list(&self, target: Option<ObjectId>) -> Result<Vec<StoredNote>> {
        let notes = self.notes();
        let names = match target {
            Some(target) => notes.list_under(&NoteName::group(target))?,
            None => notes.list()?,
        };
        let mut out = Vec::new();
        for name in names {
            let Some(note) = NoteName::parse(&name) else {
                continue;
            };
            if let Some(entry) = notes.get_entry(&name)? {
                out.push(self.stored(note.id, entry)?);
            }
        }
        out.sort_by_key(|note| note.id);
        Ok(out)
    }

    /// A single note by its identity oid. `None` when no note with that id
    /// exists. Accepts only a full oid — no prefix resolution.
    ///
    /// # Errors
    ///
    /// Propagates a ref, commit, or tree-read failure, and a malformed or
    /// unrecognized stored [`Binding`] shape.
    pub fn get(&self, id: ObjectId) -> Result<Option<StoredNote>> {
        let Some(name) = self.find(id)? else {
            return Ok(None);
        };
        match self.notes().get_entry(&name)? {
            Some(entry) => self.stored(id, entry).map(Some),
            None => Ok(None),
        }
    }

    /// Read the note document committed directly at `commit`, rather than at
    /// a ref's current tip — the version-history counterpart to
    /// [`Store::get`], for reading an older entry off [`Store::history`]'s
    /// list. `id` is the note's own identity oid, supplied by the caller
    /// rather than recovered from `commit`, since a genesis-keyed note's
    /// identity cannot be recovered from `commit` alone.
    ///
    /// # Errors
    ///
    /// Propagates a commit- or tree-read failure, and a malformed or
    /// unrecognized stored [`Binding`] shape. Does not check that `commit`
    /// is actually reachable from any note ref, nor that `id` is really the
    /// note this `commit` belongs to.
    pub fn get_at(&self, id: ObjectId, commit: ObjectId) -> Result<StoredNote> {
        let entry = self.notes().get_entry_at(commit)?;
        self.stored(id, entry)
    }

    /// Delete a note's ref. Returns whether it existed.
    ///
    /// # Errors
    ///
    /// Propagates a ref-lookup or deletion failure.
    pub fn remove(&self, id: ObjectId) -> Result<bool> {
        let Some(name) = self.find(id)? else {
            return Ok(false);
        };
        Ok(self.notes().remove(&name)?)
    }

    /// The version history (commit ids, tip-first) of a note. Empty if
    /// absent.
    ///
    /// # Errors
    ///
    /// Propagates a ref or commit-read failure.
    pub fn history(&self, id: ObjectId) -> Result<Vec<ObjectId>> {
        match self.find(id)? {
            Some(name) => Ok(self.notes().history(&name)?),
            None => Ok(Vec::new()),
        }
    }

    // ── internals ────────────────────────────────────────────────────────

    fn notes(&self) -> Kind<'_, Typed<Note>, R, O> {
        self.inner.kind(segment(NOTES_KIND))
    }

    /// [`Self::notes`] with its schema published, for the write paths.
    ///
    /// Publishing commits forward unconditionally, so this checks first —
    /// otherwise every open would mint a fresh, identical schema commit.
    fn published(&self) -> Result<Kind<'_, Typed<Note>, R, O>> {
        let notes = self.notes();
        if notes.schema().get()?.is_none() {
            notes.publish()?;
        }
        Ok(notes)
    }

    /// The entity name of the note with identity `id`, or `None` when there is
    /// none.
    ///
    /// Both identity schemes name an entity `<target>/<id>`, and `id` alone
    /// does not carry its target, so this scans the kind's ref names — never
    /// its objects — for the one whose second segment matches.
    fn find(&self, id: ObjectId) -> Result<Option<RefPath>> {
        Ok(self
            .notes()
            .list()?
            .into_iter()
            .find(|name| NoteName::parse(name).is_some_and(|note| note.id == id)))
    }

    /// Recover the domain shape of a note read back at `entry`, under the
    /// identity `id` its entity name carries.
    fn stored(&self, id: ObjectId, entry: Entry<Note>) -> Result<StoredNote> {
        let note = entry.value;
        let binding = note.binding;
        Ok(StoredNote {
            id,
            target: binding.target(),
            binding,
            body: note.body,
            message: entry.message,
            attachment: note.attachment.map(|attachment| attachment.oid()),
            parent: note.parent,
            state: note.state,
            commit: entry.commit,
            created_at: note.created_at,
        })
    }
}

/// A [`Store`] over a `gix` repository's own refs and object database.
pub type RepoStore<'r> = Store<GixRefStore<'r>, &'r gix::OdbHandle>;

impl<'r> RepoStore<'r> {
    /// Open a store over `repo` with the default `refs/anchors` prefix.
    #[must_use]
    pub fn open(repo: &'r gix::Repository) -> Self {
        let prefix = RefPrefix::new(ANCHOR_PREFIX).expect("ANCHOR_PREFIX is a valid ref prefix");
        Store::with_prefix(repo, prefix)
    }

    /// Open a store over `repo` rooted at `prefix` instead of the default —
    /// the same engine (CAS, codec, both identity schemes), a different ref
    /// namespace, so a downstream consumer (a `gix-comment` at
    /// `refs/comments`, say) gets its own non-colliding tree of refs.
    #[must_use]
    pub fn with_prefix(repo: &'r gix::Repository, prefix: RefPrefix) -> Self {
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

    fn oid_of(byte: u8) -> ObjectId {
        let digit = format!("{byte:x}");
        ObjectId::from_hex(digit.repeat(40).as_bytes()).expect("valid hex")
    }

    fn prefix() -> RefPrefix {
        RefPrefix::new(ANCHOR_PREFIX).expect("ANCHOR_PREFIX is a valid ref prefix")
    }

    fn memory_store() -> Store<MemoryRefStore, ObjectStore> {
        Store::new(MemoryRefStore::new(), ObjectStore::default(), &prefix())
    }

    // ── layout ───────────────────────────────────────────────────────────

    #[test]
    fn attach_writes_the_ref_at_prefix_target_id() {
        let store = memory_store();
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let id = store.attach(&binding, b"note", None).unwrap();

        let expected = RefName::new(format!(
            "{ANCHOR_PREFIX}/data/{NOTES_KIND}/{}/{id}",
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
        let id = store.attach(&binding, b"note", None).unwrap();

        // "refs/anchorsfoo" shares a string prefix with "refs/anchors" but is
        // not a `/`-bounded child of it.
        let foreign = RefName::new(format!(
            "refs/anchorsfoo/{}/{}",
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
        // ...but only what lives under this store's own prefix is a note.
        let notes = store.list(None).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].id, id);
    }

    /// Notes are one kind whatever they are attached to, so the schema is
    /// published once for the whole store — not once per target. This is why
    /// an entity name is allowed to nest at all.
    #[test]
    fn every_target_shares_one_published_schema() {
        let store = memory_store();
        for byte in 1..=4 {
            let binding = Binding::Commit {
                commit: oid_of(byte).into(),
            };
            store.attach(&binding, b"note", None).unwrap();
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
        assert_eq!(store.inner.kinds().unwrap(), vec![segment(NOTES_KIND)]);
    }

    /// Neither identity scheme may truncate its name: a shortened segment
    /// would put the note namespace's collision odds below the object
    /// database's own.
    #[test]
    fn both_identity_schemes_name_entities_by_full_oids() {
        let store = memory_store();
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let attached = store.attach(&binding, b"note", None).unwrap();
        let created = store
            .create(&binding, b"note", None, None, None, "create")
            .unwrap();

        for id in [attached, created] {
            let name = store.find(id).unwrap().expect("note exists");
            let [target, leaf] = name.segments() else {
                panic!("entity name is a <target>/<id> pair: {name}");
            };
            assert_eq!(target.as_str(), binding.target().to_string());
            assert_eq!(leaf.as_str(), id.to_string());
            assert_eq!(leaf.as_str().len(), 40, "full hex, not truncated");
        }
    }

    // ── round trip, both identity schemes ───────────────────────────────

    #[test]
    fn attach_then_get_round_trips_with_no_repository() {
        let store = memory_store();
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let id = store.attach(&binding, b"hello", None).unwrap();

        let note = store.get(id).unwrap().expect("note exists");
        assert_eq!(note.body, b"hello");
        assert_eq!(note.binding, binding);
        assert_eq!(note.target, binding.target());

        let listed = store.list(None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
    }

    #[test]
    fn reattach_versions_the_same_ref_forward() {
        let store = memory_store();
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let id1 = store.attach(&binding, b"v1", None).unwrap();
        let id2 = store.attach(&binding, b"v2", None).unwrap();
        assert_eq!(id1, id2, "binding-keyed: same binding, same identity");
        assert_eq!(store.history(id1).unwrap().len(), 2);
    }

    #[test]
    fn create_twice_on_the_same_binding_mints_two_distinct_refs() {
        let store = memory_store();
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let first = store.create(&binding, b"a", None, None, None, "a").unwrap();
        let second = store.create(&binding, b"b", None, None, None, "b").unwrap();
        assert_ne!(first, second, "genesis-keyed: distinct identities");
        assert_eq!(store.list(Some(binding.target())).unwrap().len(), 2);
    }

    // ── created_at forwarding ────────────────────────────────────────────

    #[test]
    fn reattach_preserves_the_original_created_at() {
        let store = memory_store();
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let id = store.attach(&binding, b"v1", None).unwrap();
        let first_created_at = store.get(id).unwrap().unwrap().created_at;

        std::thread::sleep(std::time::Duration::from_millis(2));
        store.attach(&binding, b"v2", None).unwrap();

        let second_created_at = store.get(id).unwrap().unwrap().created_at;
        assert_eq!(second_created_at, first_created_at);
    }

    #[test]
    fn update_preserves_the_original_created_at() {
        let store = memory_store();
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let id = store.create(&binding, b"a", None, None, None, "a").unwrap();
        let first_created_at = store.get(id).unwrap().unwrap().created_at;

        std::thread::sleep(std::time::Duration::from_millis(2));
        store.update(id, b"b", None, None, None, "b").unwrap();

        let second_created_at = store.get(id).unwrap().unwrap().created_at;
        assert_eq!(second_created_at, first_created_at);
    }

    // ── history ──────────────────────────────────────────────────────────

    #[test]
    fn history_lists_commits_tip_first() {
        let store = memory_store();
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let id = store.attach(&binding, b"v1", None).unwrap();
        let first_commit = store.get(id).unwrap().unwrap().commit;
        store.attach(&binding, b"v2", None).unwrap();
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

    /// The ref a note's entity lives at.
    fn note_ref<R, O>(store: &Store<R, O>, target: ObjectId, id: ObjectId) -> RefName
    where
        R: RefStore + Committer,
        O: Find + Write,
    {
        store.notes().reference(&NoteName { target, id }.path())
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
    fn a_notes_author_comes_from_the_author_identity_not_the_committer() {
        let store = Store::new(
            SplitIdentity(MemoryRefStore::new()),
            ObjectStore::default(),
            &prefix(),
        );
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let id = store.attach(&binding, b"note", None).unwrap();
        let commit = store.history(id).unwrap()[0];

        let (author, committer) = identities(store.inner.objects(), commit);
        assert_eq!(author, "Author");
        assert_eq!(committer, "Committer");
    }

    /// Commit a note the way a concurrent writer would — a real, schema-bound
    /// entity commit parented on `parent` — without going through the ref
    /// under test, so it can be scripted as a
    /// [`FlakyRefStore::push_concurrent`] winner.
    ///
    /// The scratch name is not a `<target-hex>/<id-hex>` pair, so
    /// [`Store::list`] and [`Store::find`] never see it.
    fn write_note_commit(
        store: &Store<FlakyRefStore, ObjectStore>,
        binding: &Binding,
        body: &[u8],
        created_at: u64,
        parent: Option<ObjectId>,
    ) -> ObjectId {
        let scratch = RefPath::new("scratch/winner").expect("valid entity path");
        let notes = store.published().expect("publish the notes schema");
        if let Some(parent) = parent {
            store
                .inner
                .refs()
                .apply(RefEdit::Create {
                    name: notes.reference(&scratch),
                    new: parent,
                })
                .expect("scratch ref");
        }
        let note = Note {
            body: body.to_vec(),
            binding: binding.clone(),
            attachment: None,
            parent: None,
            state: None,
            created_at,
        };
        notes
            .write(&note)
            .message("concurrent")
            .at(&scratch)
            .expect("write concurrent commit")
    }

    #[test]
    fn attach_with_attachment_retries_and_forwards_the_winners_created_at() {
        let store = flaky_store();
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let id = store.attach(&binding, b"v1", None).unwrap();
        let refname = note_ref(&store, binding.target(), id);
        let original_tip = store.inner.refs().read(&refname).unwrap().unwrap();
        let original_created_at = store.get(id).unwrap().unwrap().created_at;

        let winner_created_at = original_created_at.wrapping_add(1_000_000);
        let winner_commit = write_note_commit(
            &store,
            &binding,
            b"concurrent",
            winner_created_at,
            Some(original_tip),
        );
        store.inner.refs().push_concurrent(RefEdit::Update {
            name: refname.clone(),
            expected: original_tip,
            new: winner_commit,
        });

        let id2 = store
            .attach_with_attachment(&binding, b"v2", None, None)
            .unwrap();
        assert_eq!(id2, id);

        let after = store.get(id).unwrap().unwrap();
        assert_eq!(after.body, b"v2");
        assert_eq!(
            after.created_at, winner_created_at,
            "forwards the race winner's created_at, not a fresh timestamp"
        );
        assert_eq!(
            store.history(id).unwrap().len(),
            3,
            "original, the concurrent winner, and the retried write"
        );
    }

    #[test]
    fn update_retries_and_carries_the_winners_binding_and_created_at_forward() {
        let store = flaky_store();
        let original_binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let id = store
            .create(&original_binding, b"a", None, None, None, "a")
            .unwrap();
        let refname = note_ref(&store, original_binding.target(), id);
        let original_tip = store.inner.refs().read(&refname).unwrap().unwrap();

        // A concurrent writer's version carries a different binding and a
        // distinguishable created_at: `update` must pick both up off the
        // winning tip, not off the value read before the race.
        let winner_binding = Binding::Commit {
            commit: oid_of(2).into(),
        };
        let winner_created_at = 123_456_789;
        let winner_commit = write_note_commit(
            &store,
            &winner_binding,
            "concurrent".as_bytes(),
            winner_created_at,
            Some(original_tip),
        );
        store.inner.refs().push_concurrent(RefEdit::Update {
            name: refname.clone(),
            expected: original_tip,
            new: winner_commit,
        });

        store
            .update(id, b"edited", None, None, None, "edit")
            .unwrap();

        let after = store.get(id).unwrap().unwrap();
        assert_eq!(after.body, b"edited");
        assert_eq!(
            after.binding, winner_binding,
            "carries the race winner's binding forward"
        );
        assert_eq!(after.created_at, winner_created_at);
    }

    #[test]
    fn remove_retries_and_deletes_the_winning_tip() {
        let store = flaky_store();
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let id = store.attach(&binding, b"v1", None).unwrap();
        let refname = note_ref(&store, binding.target(), id);
        let original_tip = store.inner.refs().read(&refname).unwrap().unwrap();

        let winner_commit =
            write_note_commit(&store, &binding, b"concurrent", 42, Some(original_tip));
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
            .create(&binding, b"body", None, None, None, "msg")
            .unwrap();
        assert_eq!(store.get(id).unwrap().unwrap().body, b"body");
    }

    // ── binding-keyed identity ──────────────────────────────────────────

    /// [`Store::attach`]'s identity is exactly [`Binding::serialize_into`]'s
    /// own oid — a consumer-usable convention, not a storage-derived one, per
    /// `DEVPLAN-boundary.md` Phase 1.
    #[test]
    fn binding_keyed_identity_is_the_bindings_own_serialize_into_oid() {
        let store = memory_store();
        let binding = Binding::Commit {
            commit: oid_of(1).into(),
        };
        let id = store.attach(&binding, b"note", None).unwrap();
        let expected = binding.serialize_into(store.inner.objects()).unwrap();
        assert_eq!(id, expected);
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
        store.attach(&binding, b"note", None).unwrap();

        let schema = store
            .notes()
            .schema()
            .get()
            .unwrap()
            .expect("schema published by attach");

        assert!(
            is_anchorable(&schema),
            "notes' published schema must embed Binding's shape"
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
