//! [`Store`]: notes — arbitrary content attached to a [`Binding`]'s target —
//! persisted as Git refs and commits, git-notes style.
//!
//! Two identity schemes, chosen by write method:
//! - **Binding-keyed** ([`Store::attach`]/[`Store::attach_with_attachment`]):
//!   one ref per (target, binding-identity), keyed by the binding's own
//!   serialized tree oid. Re-attaching a binding commits a new version
//!   forward onto the same ref.
//! - **Genesis-keyed** ([`Store::create`]/[`Store::update`]): one ref per
//!   note *instance*, keyed by the oid of the parentless commit
//!   [`Store::create`] mints — never the binding's — so two notes about the
//!   same binding (a reply and what it replies to) get distinct identities.
//!   [`Store::update`] versions one forward by id.
//!
//! Every note also carries `parent` and `state`, opaque to this crate
//! (`None` for the binding-keyed scheme) — a downstream consumer like
//! `gix-comment` builds reply threads and a resolvable lifecycle on them.

use facet::Facet;
use facet_git_tree::RawTree;
use gix::ObjectId;
use gix::objs::{CommitRef, Find, FindExt, Write};
use gix_refstore::{
    ApplyError, Committer, GixRefStore, RefEdit, RefName, RefPrefix, RefSegment, RefStore,
};

use crate::binding::Binding;
use crate::error::{Error, Result};

/// [`Store::open`]'s default prefix.
const ANCHOR_PREFIX: &str = "refs/anchors";

/// The document committed at a note's ref: `body`, the [`Binding`] it is
/// attached to, an optional `attachment` tree, and opaque `parent`/`state`
/// bookkeeping — all embedded by tree id so referenced content stays
/// reachable through the note's own tree (`anchor.retention`).
#[derive(Facet)]
struct Note {
    body: Vec<u8>,
    binding: RawTree,
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

/// A note ref's two path components under a [`Store`]'s prefix:
/// `<prefix>/<target-hex>/<id-hex>`.
struct NoteRef {
    target: ObjectId,
    id: ObjectId,
}

impl NoteRef {
    fn to_ref_name(&self, prefix: &RefPrefix) -> RefName {
        prefix
            .child(&hex_segment(self.target))
            .join(&hex_segment(self.id))
    }

    /// Recover a `NoteRef` from a name known to live under `prefix`. `None`
    /// when `name` is not a `<target>/<id>` leaf directly under it.
    fn parse(name: &RefName, prefix: &RefPrefix) -> Option<NoteRef> {
        let rest = name.strip_prefix(prefix)?;
        let (target, id) = rest.split_once('/')?;
        if id.contains('/') {
            return None;
        }
        Some(NoteRef {
            target: ObjectId::from_hex(target.as_bytes()).ok()?,
            id: ObjectId::from_hex(id.as_bytes()).ok()?,
        })
    }
}

/// An [`ObjectId`]'s hex rendering is always a valid ref-name segment.
fn hex_segment(id: ObjectId) -> RefSegment {
    RefSegment::new(id.to_string()).expect("object id hex is a valid ref segment")
}

/// A content-addressed store of notes over a [`RefStore`]/[`Committer`] and
/// an object database, git-notes style: one note per identity (binding- or
/// genesis-keyed, depending on which write method is used), editable with
/// full history.
pub struct Store<R, O> {
    refs: R,
    objects: O,
    /// Where this store's refs live: `<prefix>/<target-hex>/<id-hex>`.
    prefix: RefPrefix,
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
        let id = binding.serialize_into(&self.objects)?;
        let refname = self.note_ref(target, id);

        let default_summary = format!("anchor {target}");
        let summary = message.unwrap_or(&default_summary);

        loop {
            let tip = self.refs.read(&refname).map_err(Error::git)?;
            // A re-attach forwards the original `created_at` rather than
            // resetting it, so editing a note never changes its place in a
            // caller's creation-order tiebreak. Read fresh every iteration:
            // a note that appeared between iterations must forward *its*
            // `created_at`, not a brand-new timestamp.
            let created_at = match tip {
                Some(commit) => self.created_at_at(commit)?,
                None => now_nanos(),
            };
            let note = Note {
                body: body.to_vec(),
                binding: RawTree::new(id),
                attachment: attachment.map(RawTree::new),
                parent: None,
                state: None,
                created_at,
            };
            let tree = facet_git_tree::serialize_into(&note, &self.objects)?;
            let commit = self.write_commit(summary, tree, tip)?;
            let edit = match tip {
                Some(expected) => RefEdit::Update {
                    name: refname.clone(),
                    expected,
                    new: commit,
                },
                None => RefEdit::Create {
                    name: refname.clone(),
                    new: commit,
                },
            };
            match self.refs.apply(edit) {
                Ok(()) => return Ok(id),
                Err(ApplyError::LostRace { .. }) => continue,
                Err(ApplyError::Backend(err)) => return Err(Error::git(err)),
            }
        }
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
    /// Propagates a [`Binding::serialize_into`] or `Note` serialization
    /// failure, and any underlying ref-store or object-database failure
    /// ([`Error::Git`]). [`Error::GenesisExists`] in the practically
    /// unreachable case that the minted genesis oid already names a note.
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
        let binding_id = binding.serialize_into(&self.objects)?;

        let note = Note {
            body: body.to_vec(),
            binding: RawTree::new(binding_id),
            attachment: attachment.map(RawTree::new),
            parent,
            state,
            created_at: now_nanos(),
        };
        let tree = facet_git_tree::serialize_into(&note, &self.objects)?;

        // A parentless commit written directly to the object database, with
        // no ref pointing at it yet: its own oid — unpredictable ahead of
        // time, unlike the binding-keyed scheme's deterministic id — becomes
        // this note's genesis identity once the ref below is created.
        let genesis = self.write_commit(message, tree, None)?;

        let refname = self.note_ref(target, genesis);
        loop {
            match self.refs.apply(RefEdit::Create {
                name: refname.clone(),
                new: genesis,
            }) {
                Ok(()) => return Ok(genesis),
                // `apply` reports both a genuine precondition failure and
                // transient lock contention as a lost race. Here the ref name
                // is derived from `genesis` itself, so only the ref's own
                // existence distinguishes them: present means this identity
                // is taken and retrying would spin forever; absent means we
                // merely hit contention, and the retry terminates once it
                // clears.
                Err(ApplyError::LostRace { .. }) => {
                    if self.refs.read(&refname).map_err(Error::git)?.is_some() {
                        return Err(Error::GenesisExists(genesis));
                    }
                }
                Err(ApplyError::Backend(err)) => return Err(Error::git(err)),
            }
        }
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
        let Some((refname, _)) = self.find_ref(id)? else {
            return Err(Error::Resolve(id.to_string()));
        };

        loop {
            let Some(tip) = self.refs.read(&refname).map_err(Error::git)? else {
                return Err(Error::Resolve(id.to_string()));
            };
            // Both re-read off the current tip every iteration, so a
            // version that lands between iterations is what gets carried
            // forward, not a stale read from before the loop started.
            let binding_id = self.binding_oid_at(tip)?;
            let created_at = self.created_at_at(tip)?;

            let note = Note {
                body: body.to_vec(),
                binding: RawTree::new(binding_id),
                attachment: attachment.map(RawTree::new),
                parent: parent.clone(),
                state: state.clone(),
                created_at,
            };
            let tree = facet_git_tree::serialize_into(&note, &self.objects)?;
            let commit = self.write_commit(message, tree, Some(tip))?;
            let edit = RefEdit::Update {
                name: refname.clone(),
                expected: tip,
                new: commit,
            };
            match self.refs.apply(edit) {
                Ok(()) => return Ok(id),
                Err(ApplyError::LostRace { .. }) => continue,
                Err(ApplyError::Backend(err)) => return Err(Error::git(err)),
            }
        }
    }

    /// Every stored note, or only those attached to `target` when given,
    /// sorted by id.
    ///
    /// # Errors
    ///
    /// Propagates a ref, commit, or tree-read failure, and a malformed or
    /// unrecognized stored [`Binding`] shape.
    pub fn list(&self, target: Option<ObjectId>) -> Result<Vec<StoredNote>> {
        let prefix = match target {
            Some(target) => self.prefix.child(&hex_segment(target)),
            None => self.prefix.clone(),
        };
        let mut out = Vec::new();
        for (refname, tip) in self.refs.prefixed(&prefix).map_err(Error::git)? {
            if let Some(note) = self.note_at_ref(&refname, tip)? {
                out.push(note);
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
        match self.find_ref(id)? {
            Some((refname, tip)) => self.note_at_ref(&refname, tip),
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
        self.note_at_commit(id, commit)
    }

    /// Delete a note's ref. Returns whether it existed.
    ///
    /// # Errors
    ///
    /// Propagates a ref-lookup or deletion failure.
    pub fn remove(&self, id: ObjectId) -> Result<bool> {
        let Some((refname, _)) = self.find_ref(id)? else {
            return Ok(false);
        };
        loop {
            let Some(tip) = self.refs.read(&refname).map_err(Error::git)? else {
                return Ok(false);
            };
            let edit = RefEdit::Delete {
                name: refname.clone(),
                expected: tip,
            };
            match self.refs.apply(edit) {
                Ok(()) => return Ok(true),
                // The ref moved (a concurrent attach/update) since our read
                // above: re-read and delete whatever is there now.
                Err(ApplyError::LostRace { .. }) => continue,
                Err(ApplyError::Backend(err)) => return Err(Error::git(err)),
            }
        }
    }

    /// The version history (commit ids, tip-first) of a note. Empty if
    /// absent.
    ///
    /// # Errors
    ///
    /// Propagates a ref or commit-read failure.
    pub fn history(&self, id: ObjectId) -> Result<Vec<ObjectId>> {
        match self.find_ref(id)? {
            Some((refname, _)) => self.ref_history(&refname),
            None => Ok(Vec::new()),
        }
    }

    // ── internals ────────────────────────────────────────────────────────

    /// `<prefix>/<target>/<id>`.
    fn note_ref(&self, target: ObjectId, id: ObjectId) -> RefName {
        NoteRef { target, id }.to_ref_name(&self.prefix)
    }

    /// Read the note at `refname`, known to point at `tip` — shared by
    /// [`Store::get`] (a fresh [`RefStore::read`]) and [`Store::list`] (the
    /// tip [`RefStore::prefixed`] already returned, so no second read).
    /// `None` when `refname` is not a `<target>/<id>` leaf under this
    /// store's prefix.
    fn note_at_ref(&self, refname: &RefName, tip: ObjectId) -> Result<Option<StoredNote>> {
        let Some(note_ref) = NoteRef::parse(refname, &self.prefix) else {
            return Ok(None);
        };
        self.note_at_commit(note_ref.id, tip).map(Some)
    }

    /// Read the note document committed at `commit` directly, under the
    /// given identity `id` — shared by [`Store::note_at_ref`] (`id` from the
    /// ref leaf) and [`Store::get_at`] (any commit off a note's history,
    /// `id` from the caller).
    fn note_at_commit(&self, id: ObjectId, commit: ObjectId) -> Result<StoredNote> {
        let (tree, message) = self.with_commit(commit, |c| {
            Ok((c.tree(), c.message().summary().to_string()))
        })?;
        let note: Note = facet_git_tree::deserialize(&tree, &self.objects)?;
        let binding_id = note.binding.oid();
        let binding = Binding::deserialize(&binding_id, &self.objects)?;
        let target = binding.target();
        Ok(StoredNote {
            id,
            target,
            binding,
            body: note.body,
            message,
            attachment: note.attachment.map(|attachment| attachment.oid()),
            parent: note.parent,
            state: note.state,
            commit,
            created_at: note.created_at,
        })
    }

    /// [`Note::created_at`] read back off `commit` — [`Store::attach_with_attachment`]'s
    /// and [`Store::update`]'s helper for forwarding a note's original
    /// creation order unchanged across a re-attach or a version-forward.
    fn created_at_at(&self, commit: ObjectId) -> Result<u64> {
        let tree = self.commit_tree(commit)?;
        let note: Note = facet_git_tree::deserialize(&tree, &self.objects)?;
        Ok(note.created_at)
    }

    /// The oid of the note document's own `binding` entry at `commit`,
    /// without decoding it into a full [`Binding`] — [`Store::update`]'s
    /// helper for carrying an existing note's binding forward unchanged,
    /// cheaper than round-tripping through [`Binding::deserialize`] and back
    /// through [`Binding::serialize_into`] for a value that is not changing.
    fn binding_oid_at(&self, commit: ObjectId) -> Result<ObjectId> {
        let tree = self.commit_tree(commit)?;
        let note: Note = facet_git_tree::deserialize(&tree, &self.objects)?;
        Ok(note.binding.oid())
    }

    /// The ref of the note with identity `id`, and the tip it points at,
    /// scanning every ref under this store's prefix for a matching leaf id.
    /// `None` when no such note exists.
    fn find_ref(&self, id: ObjectId) -> Result<Option<(RefName, ObjectId)>> {
        for (refname, tip) in self.refs.prefixed(&self.prefix).map_err(Error::git)? {
            if NoteRef::parse(&refname, &self.prefix).is_some_and(|note_ref| note_ref.id == id) {
                return Ok(Some((refname, tip)));
            }
        }
        Ok(None)
    }

    /// Write a commit object, without touching any ref.
    fn write_commit(
        &self,
        message: &str,
        tree: ObjectId,
        parent: Option<ObjectId>,
    ) -> Result<ObjectId> {
        let signature = self.refs.signature().map_err(Error::git)?;
        let commit = gix::objs::Commit {
            tree,
            parents: parent.into_iter().collect(),
            author: signature.clone(),
            committer: signature,
            encoding: None,
            message: message.into(),
            extra_headers: Vec::new(),
        };
        self.objects.write(&commit).map_err(Error::git)
    }

    /// First-parent walk of a ref's commits, tip-first; empty when absent.
    fn ref_history(&self, refname: &RefName) -> Result<Vec<ObjectId>> {
        let mut out = Vec::new();
        let mut cursor = self.refs.read(refname).map_err(Error::git)?;
        while let Some(id) = cursor {
            out.push(id);
            cursor = self.with_commit(id, |c| Ok(c.parents().next()))?;
        }
        Ok(out)
    }

    /// The tree of the commit `id` points at.
    fn commit_tree(&self, id: ObjectId) -> Result<ObjectId> {
        self.with_commit(id, |c| Ok(c.tree()))
    }

    /// Read an object as a commit and hand it to `f`. `Error::Git` when it
    /// is absent or not a valid commit.
    fn with_commit<T>(
        &self,
        id: ObjectId,
        f: impl FnOnce(&CommitRef<'_>) -> Result<T>,
    ) -> Result<T> {
        let mut buf = Vec::new();
        let data = self.objects.find(&id, &mut buf).map_err(Error::git)?;
        let commit = CommitRef::from_bytes(data.data, data.object_hash).map_err(Error::git)?;
        f(&commit)
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
        Store {
            refs: GixRefStore::new(repo),
            objects: &repo.objects,
            prefix,
        }
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

    use facet_git_tree::ObjectStore;
    use gix_refstore::{MemoryRefStore, Signature};

    use super::*;

    fn hex(byte: u8) -> ObjectId {
        let digit = format!("{byte:x}");
        ObjectId::from_hex(digit.repeat(40).as_bytes()).expect("valid hex")
    }

    fn prefix() -> RefPrefix {
        RefPrefix::new(ANCHOR_PREFIX).expect("ANCHOR_PREFIX is a valid ref prefix")
    }

    fn memory_store() -> Store<MemoryRefStore, ObjectStore> {
        Store {
            refs: MemoryRefStore::new(),
            objects: ObjectStore::default(),
            prefix: prefix(),
        }
    }

    // ── layout ───────────────────────────────────────────────────────────

    #[test]
    fn attach_writes_the_ref_at_prefix_target_id() {
        let store = memory_store();
        let binding = Binding::Commit { commit: hex(1) };
        let id = store.attach(&binding, b"note", None).unwrap();

        let expected =
            RefName::new(format!("{}/{}/{id}", ANCHOR_PREFIX, binding.target())).unwrap();
        assert!(store.refs.read(&expected).unwrap().is_some());
    }

    #[test]
    fn prefix_boundary_is_a_whole_segment_not_a_string_prefix() {
        let store = memory_store();
        let binding = Binding::Commit { commit: hex(1) };
        let id = store.attach(&binding, b"note", None).unwrap();

        // "refs/anchorsfoo" shares a string prefix with "refs/anchors" but is
        // not a `/`-bounded child of it.
        let foreign =
            RefName::new(format!("refs/anchorsfoo/{}/{}", binding.target(), hex(2))).unwrap();
        store
            .refs
            .apply(RefEdit::Create {
                name: foreign,
                new: hex(3),
            })
            .unwrap();

        // Both refs really exist in the backing store...
        assert_eq!(
            store
                .refs
                .prefixed(&RefPrefix::new("refs").unwrap())
                .unwrap()
                .len(),
            2
        );
        // ...but only the one under this store's own prefix is visible.
        let notes = store.list(None).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].id, id);
    }

    // ── round trip, both identity schemes ───────────────────────────────

    #[test]
    fn attach_then_get_round_trips_with_no_repository() {
        let store = memory_store();
        let binding = Binding::Commit { commit: hex(1) };
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
        let binding = Binding::Commit { commit: hex(1) };
        let id1 = store.attach(&binding, b"v1", None).unwrap();
        let id2 = store.attach(&binding, b"v2", None).unwrap();
        assert_eq!(id1, id2, "binding-keyed: same binding, same identity");
        assert_eq!(store.history(id1).unwrap().len(), 2);
    }

    #[test]
    fn create_twice_on_the_same_binding_mints_two_distinct_refs() {
        let store = memory_store();
        let binding = Binding::Commit { commit: hex(1) };
        let first = store.create(&binding, b"a", None, None, None, "a").unwrap();
        let second = store.create(&binding, b"b", None, None, None, "b").unwrap();
        assert_ne!(first, second, "genesis-keyed: distinct identities");
        assert_eq!(store.list(Some(binding.target())).unwrap().len(), 2);
    }

    // ── created_at forwarding ────────────────────────────────────────────

    #[test]
    fn reattach_preserves_the_original_created_at() {
        let store = memory_store();
        let binding = Binding::Commit { commit: hex(1) };
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
        let binding = Binding::Commit { commit: hex(1) };
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
        let binding = Binding::Commit { commit: hex(1) };
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
        /// Create a ref at whatever name the caller's own edit targets,
        /// using a placeholder object, before delegating — for when that
        /// name is not known ahead of time (`create`'s genesis-derived ref).
        Collide,
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

        fn push_collide(&self) {
            self.injections.borrow_mut().push_back(Injection::Collide);
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
                Some(Injection::Collide) => {
                    let collide = RefEdit::Create {
                        name: edit.name().clone(),
                        new: hex(0xA),
                    };
                    self.inner
                        .apply(collide)
                        .expect("injected collision applies cleanly");
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
        Store {
            refs: FlakyRefStore::new(),
            objects: ObjectStore::default(),
            prefix: prefix(),
        }
    }

    /// Writes a note commit directly, bypassing every [`Store`] write
    /// method — stands in for a concurrent writer's version when scripting
    /// a [`FlakyRefStore::push_concurrent`] injection.
    fn write_note_commit(
        store: &Store<FlakyRefStore, ObjectStore>,
        binding_id: ObjectId,
        body: &[u8],
        created_at: u64,
        parent: Option<ObjectId>,
    ) -> ObjectId {
        let note = Note {
            body: body.to_vec(),
            binding: RawTree::new(binding_id),
            attachment: None,
            parent: None,
            state: None,
            created_at,
        };
        let tree = facet_git_tree::serialize_into(&note, &store.objects).expect("serialize note");
        store
            .write_commit("concurrent", tree, parent)
            .expect("write concurrent commit")
    }

    #[test]
    fn attach_with_attachment_retries_and_forwards_the_winners_created_at() {
        let store = flaky_store();
        let binding = Binding::Commit { commit: hex(1) };
        let id = store.attach(&binding, b"v1", None).unwrap();
        let refname = store.note_ref(binding.target(), id);
        let original_tip = store.refs.read(&refname).unwrap().unwrap();
        let original_created_at = store.get(id).unwrap().unwrap().created_at;

        let winner_created_at = original_created_at.wrapping_add(1_000_000);
        let winner_commit = write_note_commit(
            &store,
            id,
            b"concurrent",
            winner_created_at,
            Some(original_tip),
        );
        store.refs.push_concurrent(RefEdit::Update {
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
        let original_binding = Binding::Commit { commit: hex(1) };
        let id = store
            .create(&original_binding, b"a", None, None, None, "a")
            .unwrap();
        let refname = store.note_ref(original_binding.target(), id);
        let original_tip = store.refs.read(&refname).unwrap().unwrap();

        // A concurrent writer's version carries a different binding and a
        // distinguishable created_at: `update` must pick both up off the
        // winning tip, not off the value read before the race.
        let winner_binding = Binding::Commit { commit: hex(2) };
        let winner_binding_id = winner_binding.serialize_into(&store.objects).unwrap();
        let winner_created_at = 123_456_789;
        let winner_commit = write_note_commit(
            &store,
            winner_binding_id,
            "concurrent".as_bytes(),
            winner_created_at,
            Some(original_tip),
        );
        store.refs.push_concurrent(RefEdit::Update {
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
        let binding = Binding::Commit { commit: hex(1) };
        let id = store.attach(&binding, b"v1", None).unwrap();
        let refname = store.note_ref(binding.target(), id);
        let original_tip = store.refs.read(&refname).unwrap().unwrap();

        let winner_commit = write_note_commit(&store, id, b"concurrent", 42, Some(original_tip));
        store.refs.push_concurrent(RefEdit::Update {
            name: refname.clone(),
            expected: original_tip,
            new: winner_commit,
        });

        assert!(store.remove(id).unwrap());
        assert!(store.get(id).unwrap().is_none());
        assert!(store.refs.read(&refname).unwrap().is_none());
    }

    #[test]
    fn create_returns_genesis_exists_when_the_ref_is_present_after_a_lost_race() {
        let store = flaky_store();
        let binding = Binding::Commit { commit: hex(1) };
        store.refs.push_collide();

        let err = store
            .create(&binding, b"body", None, None, None, "msg")
            .unwrap_err();
        assert!(matches!(err, Error::GenesisExists(_)));
    }

    #[test]
    fn create_retries_to_success_on_pure_contention() {
        let store = flaky_store();
        let binding = Binding::Commit { commit: hex(1) };
        store.refs.push_phantom();

        let id = store
            .create(&binding, b"body", None, None, None, "msg")
            .unwrap();
        assert_eq!(store.get(id).unwrap().unwrap().body, b"body");
    }
}
