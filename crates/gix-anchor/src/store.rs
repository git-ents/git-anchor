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

use std::time::Duration;

use facet::Facet;
use facet_git_tree::RawTree;
use gix::ObjectId;
use gix::refs::transaction::PreviousValue;
use gix_refstore::{RefName, RefPrefix, RefSegment};

use crate::binding::Binding;
use crate::error::{Error, Result};

/// [`Store::open`]'s default prefix.
const ANCHOR_PREFIX: &str = "refs/anchors";
/// Our per-ref lock files live under `<git-dir>/<LOCK_DIR>/`, kept separate
/// from git's own `<ref>.lock` so holding one never blocks gix's own ref
/// transaction (which would deadlock against us).
const LOCK_DIR: &str = "gix-anchor-locks";
/// How long to block, with backoff, for a contended per-ref lock before
/// giving up.
const LOCK_TIMEOUT: Duration = Duration::from_secs(30);
/// A belt-and-suspenders bound on retries once the lock is held; serialized
/// writers should land on the first attempt.
const MAX_CAS_ATTEMPTS: u32 = 8;

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

/// A content-addressed store of notes over a `gix` repository, git-notes
/// style: one note per identity (binding- or genesis-keyed, depending on
/// which write method is used), editable with full history.
pub struct Store<'r> {
    repo: &'r gix::Repository,
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

impl<'r> Store<'r> {
    /// Open a store over `repo` with the default `refs/anchors` prefix.
    #[must_use]
    pub fn open(repo: &'r gix::Repository) -> Store<'r> {
        let prefix = RefPrefix::new(ANCHOR_PREFIX).expect("ANCHOR_PREFIX is a valid ref prefix");
        Store::with_prefix(repo, prefix)
    }

    /// Open a store over `repo` rooted at `prefix` instead of the default —
    /// the same engine (locking, CAS, codec, both identity schemes), a
    /// different ref namespace, so a downstream consumer (a `gix-comment` at
    /// `refs/comments`, say) gets its own non-colliding tree of refs.
    #[must_use]
    pub fn with_prefix(repo: &'r gix::Repository, prefix: RefPrefix) -> Store<'r> {
        Store { repo, prefix }
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
    /// failure, and [`Error::CasExhausted`] when the per-ref compare-and-swap
    /// stays contended past the retry budget.
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
        let id = binding.serialize_into(&self.repo.objects)?;

        let refname = self.note_ref(target, id);
        // A re-attach forwards the original `created_at` rather than
        // resetting it, so editing a note never changes its place in a
        // caller's creation-order tiebreak.
        let created_at = match self.tip(&refname)? {
            Some(tip) => self.created_at_at(tip)?,
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
        let tree = facet_git_tree::serialize_into(&note, &self.repo.objects)?;

        let default_summary = format!("anchor {target}");
        let summary = message.unwrap_or(&default_summary);
        self.commit_forward(&refname, summary, tree)?;
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
    /// Propagates a [`Binding::serialize_into`] or `Note` serialization
    /// failure, and any underlying `gix` commit- or ref-write failure
    /// ([`Error::Git`]).
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
        let binding_id = binding.serialize_into(&self.repo.objects)?;

        let note = Note {
            body: body.to_vec(),
            binding: RawTree::new(binding_id),
            attachment: attachment.map(RawTree::new),
            parent,
            state,
            created_at: now_nanos(),
        };
        let tree = facet_git_tree::serialize_into(&note, &self.repo.objects)?;

        // A parentless commit written directly to the object database, with
        // no ref pointing at it yet: its own oid — unpredictable ahead of
        // time, unlike the binding-keyed scheme's deterministic id — becomes
        // this note's genesis identity once the ref below is created.
        let commit = self
            .repo
            .new_commit(message, tree, std::iter::empty::<ObjectId>())
            .map_err(Error::git)?;
        let genesis = commit.id;

        let refname = self.note_ref(target, genesis);
        self.repo
            .reference(
                refname.as_str(),
                genesis,
                PreviousValue::MustNotExist,
                message,
            )
            .map_err(Error::git)?;

        Ok(genesis)
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
    /// propagates a `Note` serialization failure or [`Error::CasExhausted`]
    /// when the per-ref compare-and-swap stays contended past the retry
    /// budget.
    pub fn update(
        &self,
        id: ObjectId,
        body: &[u8],
        attachment: Option<ObjectId>,
        parent: Option<String>,
        state: Option<String>,
        message: &str,
    ) -> Result<ObjectId> {
        let Some(refname) = self.find_ref(id)? else {
            return Err(Error::Resolve(id.to_string()));
        };
        let Some(tip) = self.tip(&refname)? else {
            return Err(Error::Resolve(id.to_string()));
        };
        let binding_id = self.binding_oid_at(tip)?;

        let note = Note {
            body: body.to_vec(),
            binding: RawTree::new(binding_id),
            attachment: attachment.map(RawTree::new),
            parent,
            state,
            created_at: self.created_at_at(tip)?,
        };
        let tree = facet_git_tree::serialize_into(&note, &self.repo.objects)?;
        self.commit_forward(&refname, message, tree)?;
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
        let prefix = match target {
            Some(target) => self.prefix.child(&hex_segment(target)),
            None => self.prefix.clone(),
        };
        let mut out = Vec::new();
        for refname in self.refs_under(&prefix)? {
            if let Some(note) = self.read_note(&refname)? {
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
            Some(refname) => self.read_note(&refname),
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
        let Some(refname) = self.find_ref(id)? else {
            return Ok(false);
        };
        match self
            .repo
            .try_find_reference(refname.as_str())
            .map_err(Error::git)?
        {
            Some(reference) => {
                reference.delete().map_err(Error::git)?;
                Ok(true)
            }
            None => Ok(false),
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
            Some(refname) => self.ref_history(&refname),
            None => Ok(Vec::new()),
        }
    }

    // ── internals ────────────────────────────────────────────────────────

    /// `<prefix>/<target>/<id>`.
    fn note_ref(&self, target: ObjectId, id: ObjectId) -> RefName {
        NoteRef { target, id }.to_ref_name(&self.prefix)
    }

    /// The current object a ref points at, or `None` when the ref is absent.
    fn tip(&self, refname: &RefName) -> Result<Option<ObjectId>> {
        match self
            .repo
            .try_find_reference(refname.as_str())
            .map_err(Error::git)?
        {
            Some(mut reference) => {
                let id = reference.peel_to_id().map_err(Error::git)?;
                Ok(Some(id.detach()))
            }
            None => Ok(None),
        }
    }

    /// Read the note at `refname`'s tip, or `None` when the ref is absent or
    /// not a `<target>/<id>` leaf under this store's prefix.
    fn read_note(&self, refname: &RefName) -> Result<Option<StoredNote>> {
        let Some(tip) = self.tip(refname)? else {
            return Ok(None);
        };
        let Some(note_ref) = NoteRef::parse(refname, &self.prefix) else {
            return Ok(None);
        };
        self.note_at_commit(note_ref.id, tip).map(Some)
    }

    /// Read the note document committed at `commit` directly, under the
    /// given identity `id` — shared by [`Store::read_note`] (a ref's tip,
    /// `id` from the ref leaf) and [`Store::get_at`] (any commit off a
    /// note's history, `id` from the caller).
    fn note_at_commit(&self, id: ObjectId, commit: ObjectId) -> Result<StoredNote> {
        let commit_obj = self.repo.find_commit(commit).map_err(Error::git)?;
        let tree = commit_obj.tree_id().map_err(Error::git)?.detach();
        let note: Note = facet_git_tree::deserialize(&tree, &self.repo.objects)?;
        let binding_id = note.binding.oid();
        let binding = Binding::deserialize(&binding_id, &self.repo.objects)?;
        let target = binding.target();
        let message = gix_object::commit::MessageRef::from_bytes(commit_obj.message_raw_sloppy())
            .summary()
            .to_string();
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
        let commit_obj = self.repo.find_commit(commit).map_err(Error::git)?;
        let tree = commit_obj.tree_id().map_err(Error::git)?.detach();
        let note: Note = facet_git_tree::deserialize(&tree, &self.repo.objects)?;
        Ok(note.created_at)
    }

    /// The oid of the note document's own `binding` entry at `commit`,
    /// without decoding it into a full [`Binding`] — [`Store::update`]'s
    /// helper for carrying an existing note's binding forward unchanged,
    /// cheaper than round-tripping through [`Binding::deserialize`] and back
    /// through [`Binding::serialize_into`] for a value that is not changing.
    fn binding_oid_at(&self, commit: ObjectId) -> Result<ObjectId> {
        let commit_obj = self.repo.find_commit(commit).map_err(Error::git)?;
        let tree = commit_obj.tree_id().map_err(Error::git)?.detach();
        let note: Note = facet_git_tree::deserialize(&tree, &self.repo.objects)?;
        Ok(note.binding.oid())
    }

    /// The full refname of the note with identity `id`, scanning every ref
    /// under this store's prefix for a matching leaf id. `None` when no such
    /// note exists.
    fn find_ref(&self, id: ObjectId) -> Result<Option<RefName>> {
        for refname in self.refs_under(&self.prefix)? {
            if NoteRef::parse(&refname, &self.prefix).is_some_and(|note_ref| note_ref.id == id) {
                return Ok(Some(refname));
            }
        }
        Ok(None)
    }

    /// Commit `tree` forward over `refname`'s current tip, under a per-ref
    /// lock so concurrent writers serialize into a fast-forward instead of
    /// one orphaning the other's commit. The retry loop then only guards a
    /// transient error while the lock is held.
    fn commit_forward(&self, refname: &RefName, msg: &str, tree: ObjectId) -> Result<ObjectId> {
        let _lock = self.lock_ref(refname)?;
        let mut attempts = 0;
        loop {
            let parent = self.tip(refname)?;
            match self.repo.commit(refname.as_str(), msg, tree, parent) {
                Ok(id) => return Ok(id.detach()),
                Err(err) if is_retryable(&err) => {
                    attempts += 1;
                    if attempts >= MAX_CAS_ATTEMPTS {
                        return Err(Error::CasExhausted {
                            refname: refname.to_string(),
                            attempts,
                        });
                    }
                }
                Err(err) => return Err(Error::git(err)),
            }
        }
    }

    /// Acquire the exclusive per-ref lock, blocking with backoff up to
    /// [`LOCK_TIMEOUT`]. The returned marker holds the lock until dropped.
    /// Lives under `<git-dir>/<LOCK_DIR>/`, deliberately not `<ref>.lock`
    /// (git's own), so our serialization never contends with gix's own ref
    /// transaction. A real on-disk lock, so separate processes serialize too.
    fn lock_ref(&self, refname: &RefName) -> Result<gix::lock::Marker> {
        // Pre-create the lock directory once and leave it in place: letting
        // the lock's rollback remove empty parents races the next writer's
        // creation of the same directory. With the directory persistent,
        // only the `.lock` files themselves churn.
        let dir = self.repo.git_dir().join(LOCK_DIR);
        std::fs::create_dir_all(&dir).map_err(Error::git)?;
        gix::lock::Marker::acquire_to_hold_resource(
            dir.join(encode_ref(refname.as_str())),
            gix::lock::acquire::Fail::AfterDurationWithBackoff(LOCK_TIMEOUT),
            None,
        )
        .map_err(Error::git)
    }

    /// First-parent walk of a ref's commits, tip-first; empty when absent.
    fn ref_history(&self, refname: &RefName) -> Result<Vec<ObjectId>> {
        let mut out = Vec::new();
        let mut cursor = self.tip(refname)?;
        while let Some(id) = cursor {
            out.push(id);
            let commit = self.repo.find_commit(id).map_err(Error::git)?;
            cursor = commit.parent_ids().next().map(|id| id.detach());
        }
        Ok(out)
    }

    /// Every ref directly or indirectly under `prefix`, sorted. A trailing
    /// `/` bounds the match to a whole segment, so `refs/anchorsfoo` never
    /// matches a `refs/anchors` prefix.
    fn refs_under(&self, prefix: &RefPrefix) -> Result<Vec<RefName>> {
        let platform = self.repo.references().map_err(Error::git)?;
        let pattern = format!("{prefix}/");
        let mut out = Vec::new();
        for reference in platform.prefixed(pattern.as_str()).map_err(Error::git)? {
            let reference = reference.map_err(Error::git)?;
            if let Ok(name) = std::str::from_utf8(reference.name().as_bstr())
                && let Ok(name) = RefName::new(name)
            {
                out.push(name);
            }
        }
        out.sort();
        Ok(out)
    }
}

/// A flat, filesystem-safe lock filename for a ref: `%` and `/` are
/// percent-escaped so the whole ref becomes one path segment, never a nested
/// directory tree.
fn encode_ref(refname: &str) -> String {
    refname.replace('%', "%25").replace('/', "%2F")
}

/// Whether a failed commit should be retried by re-reading the tip: either a
/// lost compare-and-swap race — the ref moved (or appeared) between our tip
/// read and the ref transaction — or contention on the ref lock itself while
/// another writer held it. Both resolve on retry; any other error is
/// genuine.
fn is_retryable(err: &gix::commit::Error) -> bool {
    use gix::refs::file::transaction::prepare::Error as Prepare;
    matches!(
        err,
        gix::commit::Error::ReferenceEdit(gix::reference::edit::Error::FileTransactionPrepare(
            Prepare::ReferenceOutOfDate { .. }
                | Prepare::MustNotExist { .. }
                | Prepare::LockAcquire { .. }
                | Prepare::PackedTransactionAcquire(_)
        ))
    )
}
