//! [`Store`]: notes — arbitrary content attached to a [`Binding`]'s
//! target — persisted as Git refs and commits, git-notes style.
//!
//! Two identity schemes share this one engine, selected by which write
//! method a caller uses:
//!
//! - **Binding-keyed** ([`Store::attach`] / [`Store::attach_with_attachment`]):
//!   one ref per (target, binding-identity) pair, at
//!   `<prefix>/<target-hex>/<binding-oid-hex>` — attaching again to the same
//!   binding commits a new version forward onto the same ref, so editing a
//!   note keeps its full history rather than overwriting it. The identity
//!   oid is the binding's own serialized tree oid — deterministic and
//!   content-addressed on the binding, not on the attached body — so the
//!   same binding always resolves to the same ref regardless of which
//!   process attached it first.
//! - **Genesis-keyed** ([`Store::create`] / [`Store::update`]): one ref per
//!   note *instance*, at `<prefix>/<target-hex>/<genesis-commit-oid>`. The
//!   identity oid is the oid of the parentless commit [`Store::create`]
//!   writes — never the binding's — so two notes about the same binding (a
//!   reply and the comment it replies to, say) get distinct identities
//!   instead of colliding onto one ref. [`Store::update`] commits a new
//!   version forward by that id, the genesis-keyed counterpart to
//!   re-[`Store::attach`]ing.
//!
//! [`Store::with_prefix`] picks where either scheme's refs live;
//! [`Store::open`] is shorthand for `with_prefix(repo, "refs/anchors")`. Every
//! note document also carries two fields, `parent` and `state`, that this
//! crate treats as opaque (`None` for [`Store::attach`] /
//! [`Store::attach_with_attachment`]) — a downstream consumer such as
//! `gix-comment` uses them to build reply threads and a resolvable lifecycle
//! on top of this one storage engine, without this crate needing to know
//! their vocabulary.

use std::time::Duration;

use facet::Facet;
use facet_git_tree::RawTree;
use gix::ObjectId;
use gix::refs::transaction::PreviousValue;

use crate::binding::Binding;
use crate::error::{Error, Result};
use crate::refname::check_hex_component;

/// [`Store::open`]'s default prefix: `refs/anchors/<target-hex>/<id-hex>`.
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

/// The document committed at a note's ref: an arbitrary attached `body`, the
/// [`Binding`] it is attached to, an optional `attachment` tree, and two
/// opaque bookkeeping fields — all embedded by tree id (`anchor.retention`)
/// so the anchor's own content and context blobs, and any attached tree,
/// stay reachable through the note's own tree.
///
/// `attachment` is opaque to this crate: it is an arbitrary tree the caller
/// hands to [`Store::attach_with_attachment`] (or, genesis-keyed,
/// [`Store::create`]/[`Store::update`]), embedded verbatim as a [`RawTree`]
/// passthrough so it survives gc through the note's ref the same way the
/// binding's own blobs do. A downstream consumer (a `Comment`, say) uses it
/// to hang extra content off a note without this crate needing to know that
/// content's shape.
///
/// `parent` and `state` are likewise opaque: [`Store::create`] and
/// [`Store::update`] pass them through verbatim as caller-supplied strings —
/// conventionally an upstream note's hex id, and a free-form lifecycle tag,
/// respectively — never interpreting them. [`Store::attach`] and
/// [`Store::attach_with_attachment`] always write `None` for both; the
/// fields exist for a genesis-keyed caller's reply/resolve vocabulary, not
/// the binding-keyed scheme.
#[derive(Facet)]
struct Note {
    body: Vec<u8>,
    binding: RawTree,
    attachment: Option<RawTree>,
    parent: Option<String>,
    state: Option<String>,
    /// Nanoseconds since the Unix epoch, best-effort, set once when a note
    /// is first created and forwarded unchanged by every later version
    /// ([`Store::attach`]/[`Store::attach_with_attachment`] re-attaching,
    /// [`Store::update`] versioning forward) — a tiebreaker for two notes
    /// whose commit author time lands in the same second (git's own
    /// resolution), which an id-based tiebreak cannot order correctly since
    /// a note's id is a content hash, uncorrelated with when it was
    /// written. Not a substitute for a note's real timestamp; a caller
    /// wanting that reads it off the storage commit itself, same as today.
    created_at: u64,
}

/// The current wall-clock time, in nanoseconds since the Unix epoch,
/// best-effort (`0` if the clock reads before the epoch) — [`Note::created_at`]'s
/// source for a freshly created note.
fn now_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// A content-addressed store of notes over a `gix` repository, git-notes
/// style: one note per identity (binding- or genesis-keyed, depending on
/// which write method is used), editable with full history.
pub struct Store<'r> {
    repo: &'r gix::Repository,
    /// Where this store's refs live: `<prefix>/<target-hex>/<id-hex>`.
    /// [`Store::open`] fixes this at [`ANCHOR_PREFIX`]; [`Store::with_prefix`]
    /// lets a caller such as `gix-comment` root the same engine at its own
    /// namespace (`refs/comments`) instead.
    prefix: &'static str,
}

/// A note read back from the [`Store`].
pub struct StoredNote {
    /// The note's identity oid — the ref-path leaf. For a binding-keyed note
    /// ([`Store::attach`] / [`Store::attach_with_attachment`]) this equals
    /// the binding's own serialized tree oid; for a genesis-keyed note
    /// ([`Store::create`] / [`Store::update`]) it is the parentless commit
    /// oid [`Store::create`] minted, independent of the binding.
    pub id: ObjectId,
    /// [`Binding::target`] — the ref-path grouping key.
    pub target: ObjectId,
    /// The binding this note is attached to, recovered from storage.
    pub binding: Binding,
    /// The arbitrary content attached to `binding`.
    pub body: Vec<u8>,
    /// The latest version's commit summary (first line of the message).
    pub message: String,
    /// The optional attachment tree embedded alongside the note, as handed
    /// to [`Store::attach_with_attachment`] (or [`Store::create`] /
    /// [`Store::update`]) — `None` for a note attached or created with no
    /// attachment.
    pub attachment: Option<ObjectId>,
    /// An upstream note's id (conventionally hex), opaque to this crate —
    /// `None` for a binding-keyed note, or a genesis-keyed note with no
    /// parent. A caller such as `gix-comment` uses this to link a reply to
    /// what it replies to.
    pub parent: Option<String>,
    /// A free-form lifecycle tag, opaque to this crate — `None` unless a
    /// caller such as `gix-comment` set one (an open/resolved state, say)
    /// via [`Store::create`] or [`Store::update`].
    pub state: Option<String>,
    /// The commit this note was read from — a note ref's tip for
    /// [`Store::get`] / [`Store::list`], or the requested commit for
    /// [`Store::get_at`]. Its author and time are the note's author and
    /// timestamp, since a note *is* a git commit.
    pub commit: ObjectId,
    /// [`Note::created_at`]: nanoseconds since the Unix epoch, best-effort,
    /// fixed at the note's first version and forwarded unchanged by every
    /// later one. A finer-grained tiebreaker than `commit`'s own author
    /// time (git's one-second resolution) for a caller such as
    /// `gix-comment` ordering notes that landed in the same second; not a
    /// substitute for `commit`'s real timestamp.
    pub created_at: u64,
}

impl<'r> Store<'r> {
    /// Open a store over `repo` with the default `refs/anchors` prefix.
    #[must_use]
    pub fn open(repo: &'r gix::Repository) -> Store<'r> {
        Store::with_prefix(repo, ANCHOR_PREFIX)
    }

    /// Open a store over `repo` rooted at `prefix` instead of the default
    /// `refs/anchors` — the same engine (locking, CAS, codec, both identity
    /// schemes), a different ref namespace, so a downstream consumer (a
    /// `gix-comment` at `refs/comments`, say) gets its own non-colliding tree
    /// of refs without duplicating any of this crate's storage logic.
    #[must_use]
    pub fn with_prefix(repo: &'r gix::Repository, prefix: &'static str) -> Store<'r> {
        Store { repo, prefix }
    }

    /// Attach `body` to the object `binding` names, git-notes style: one
    /// note per (target, binding-identity), editable with history.
    ///
    /// The identity oid — `binding`'s own serialized tree oid — is
    /// deterministic and content-addressed on the binding alone, never on
    /// `body`, so re-attaching to the same binding commits a new version
    /// forward onto the same ref (`<prefix>/<target>/<id>`) instead of
    /// forking it. `message` sets the commit summary; when `None`, a default
    /// `anchor <target>` summary is used.
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
    /// note document as a [`RawTree`] passthrough (`anchor.retention`): the
    /// tree stays reachable through the note's own ref regardless of what
    /// else references it, the same way the binding's own blobs do.
    ///
    /// `attachment` must already exist in the repository's object database
    /// (a [`RawTree`] carries no content of its own to write) — callers
    /// resolve it however they like, e.g. `rev_parse` of a tree-ish. It is
    /// opaque here: this crate never decodes it. A downstream consumer such
    /// as a `Comment` uses it to hang extra content off a note.
    ///
    /// Returns the note's identity oid, exactly as [`Store::attach`] does —
    /// the attachment is part of the note's *body document*, not its
    /// identity, so re-attaching the same binding with a different attachment
    /// still commits forward onto the same ref. `parent` and `state` are
    /// always written `None` — the binding-keyed scheme has no use for
    /// either; a caller that needs them wants [`Store::create`] instead.
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
        check_hex_component("target", &target.to_string())?;
        check_hex_component("id", &id.to_string())?;

        let refname = self.anchor_ref(target, id);
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
    /// `binding` names, but under a *fresh* identity rather than the
    /// binding's own oid.
    ///
    /// The identity is the oid of the parentless commit this method writes
    /// — never derived from `binding` — so calling this twice with the same
    /// binding creates two distinct notes at two distinct refs
    /// (`<prefix>/<target>/<genesis-1>`, `<prefix>/<target>/<genesis-2>`)
    /// rather than versioning one forward. That is the point: a caller such
    /// as `gix-comment` uses it so a reply and the comment it replies to —
    /// both about the same binding — never collide onto one ref, and two
    /// people can comment on the same line without contending for the same
    /// identity. Re-editing a genesis-keyed note by id is
    /// [`Store::update`]'s job, not this one's.
    ///
    /// `message` sets the commit summary (unlike [`Store::attach`], there is
    /// no default to fall back to — a genesis-keyed caller always has one to
    /// give, e.g. a comment's own first line).
    ///
    /// Returns the new note's identity oid (the genesis commit's own oid).
    ///
    /// # Errors
    ///
    /// Propagates a [`Binding::serialize_into`] or `Note` serialization
    /// failure, any underlying `gix` commit- or ref-write failure
    /// ([`Error::Git`]), and [`Error::InvalidRefComponent`] on the
    /// (practically unreachable) chance `target` or the minted genesis oid
    /// cannot be used as a ref-name segment.
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
        check_hex_component("target", &target.to_string())?;
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
        check_hex_component("id", &genesis.to_string())?;

        let refname = self.anchor_ref(target, genesis);
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
    /// same identity, a fresh `body`/`attachment`/`parent`/`state`, full
    /// history preserved. The note's binding is carried forward unchanged
    /// (it is read back off the ref's current tip); this method has no way
    /// to change what a note is *about*, only its content.
    ///
    /// Returns `id` unchanged.
    ///
    /// # Errors
    ///
    /// [`Error::Resolve`] when no note with `id` exists — `update` names an
    /// existing note, unlike [`Store::create`], which always makes a new
    /// one. Otherwise propagates a `Note` serialization failure or
    /// [`Error::CasExhausted`] when the per-ref compare-and-swap stays
    /// contended past the retry budget.
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
            Some(target) => format!("{}/{target}/", self.prefix),
            None => format!("{}/", self.prefix),
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

    /// Read the note document committed directly at `commit`, rather than
    /// at a ref's current tip — the version-history counterpart to
    /// [`Store::get`], for reading an older entry off [`Store::history`]'s
    /// list (`git anchor show <id>~N`'s library hook). `id` is the note's own
    /// identity oid (the ref leaf), supplied by the caller rather than
    /// recomputed, since a genesis-keyed note's identity cannot be recovered
    /// from `commit` alone (unlike a binding-keyed note's, which is the
    /// binding's own oid).
    ///
    /// # Errors
    ///
    /// Propagates a commit- or tree-read failure, and a malformed or
    /// unrecognized stored [`Binding`] shape. Does not check that `commit`
    /// is actually reachable from any note ref, nor that `id` is really the
    /// note this `commit` belongs to — callers that need those guarantees
    /// should check against [`Store::history`] themselves.
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
    fn anchor_ref(&self, target: ObjectId, id: ObjectId) -> String {
        format!("{}/{target}/{id}", self.prefix)
    }

    /// The current object a ref points at, or `None` when the ref is absent.
    fn tip(&self, refname: &str) -> Result<Option<ObjectId>> {
        match self.repo.try_find_reference(refname).map_err(Error::git)? {
            Some(mut reference) => {
                let id = reference.peel_to_id().map_err(Error::git)?;
                Ok(Some(id.detach()))
            }
            None => Ok(None),
        }
    }

    /// Read the note at `refname`'s tip, or `None` when the ref is absent.
    /// The note's identity is the ref's own leaf segment, not anything
    /// recomputed from its content — the same oid [`Store::find_ref`]
    /// matched to land here, and, for a binding-keyed note, numerically
    /// identical to the binding's own serialized tree oid anyway.
    fn read_note(&self, refname: &str) -> Result<Option<StoredNote>> {
        let Some(tip) = self.tip(refname)? else {
            return Ok(None);
        };
        let id = leaf_id(refname)?;
        self.note_at_commit(id, tip).map(Some)
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
    /// under this store's prefix for a matching leaf segment. `None` when no
    /// such note exists.
    fn find_ref(&self, id: ObjectId) -> Result<Option<String>> {
        let leaf = id.to_string();
        for refname in self.refs_under(&format!("{}/", self.prefix))? {
            if refname.rsplit('/').next() == Some(leaf.as_str()) {
                return Ok(Some(refname));
            }
        }
        Ok(None)
    }

    /// Commit `tree` forward over the current tip of `refname`, under a
    /// per-ref lock so writers serialize instead of forking the ref.
    ///
    /// `gix::Repository::commit` derives the ref transaction's
    /// expected-previous value from the first parent —
    /// `ExistingMustMatch` on a named ref, `MustNotExist` when parentless —
    /// but that precondition alone does not stop two concurrent writers from
    /// both appending to the same tip and orphaning one commit. Holding
    /// [`lock_ref`](Self::lock_ref) around the tip read and the commit makes
    /// each write a fast-forward, so history stays linear across threads and
    /// processes. The retry loop is then just a guard against a transient
    /// error while the lock is held.
    fn commit_forward(&self, refname: &str, msg: &str, tree: ObjectId) -> Result<ObjectId> {
        let _lock = self.lock_ref(refname)?;
        let mut attempts = 0;
        loop {
            let parent = self.tip(refname)?;
            match self.repo.commit(refname, msg, tree, parent) {
                Ok(id) => return Ok(id.detach()),
                Err(err) if is_retryable(&err) => {
                    attempts += 1;
                    if attempts >= MAX_CAS_ATTEMPTS {
                        return Err(Error::CasExhausted {
                            refname: refname.to_owned(),
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
    ///
    /// The lock resource lives under `<git-dir>/<LOCK_DIR>/` — deliberately
    /// not `<ref>.lock`, which git itself uses — so our serialization never
    /// contends with gix's own ref transaction. It is a real on-disk lock, so
    /// separate processes serialize too.
    fn lock_ref(&self, refname: &str) -> Result<gix::lock::Marker> {
        // Pre-create the lock directory once and leave it in place: letting
        // the lock's rollback remove empty parents races the next writer's
        // creation of the same directory. With the directory persistent,
        // only the `.lock` files themselves churn.
        let dir = self.repo.git_dir().join(LOCK_DIR);
        std::fs::create_dir_all(&dir).map_err(Error::git)?;
        gix::lock::Marker::acquire_to_hold_resource(
            dir.join(encode_ref(refname)),
            gix::lock::acquire::Fail::AfterDurationWithBackoff(LOCK_TIMEOUT),
            None,
        )
        .map_err(Error::git)
    }

    /// First-parent walk of a ref's commits, tip-first; empty when absent.
    fn ref_history(&self, refname: &str) -> Result<Vec<ObjectId>> {
        let mut out = Vec::new();
        let mut cursor = self.tip(refname)?;
        while let Some(id) = cursor {
            out.push(id);
            let commit = self.repo.find_commit(id).map_err(Error::git)?;
            cursor = commit.parent_ids().next().map(|id| id.detach());
        }
        Ok(out)
    }

    /// Every ref (full name) directly or indirectly under `prefix`, sorted.
    fn refs_under(&self, prefix: &str) -> Result<Vec<String>> {
        let platform = self.repo.references().map_err(Error::git)?;
        let mut out = Vec::new();
        for reference in platform.prefixed(prefix).map_err(Error::git)? {
            let reference = reference.map_err(Error::git)?;
            if let Ok(name) = std::str::from_utf8(reference.name().as_bstr()) {
                out.push(name.to_owned());
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

/// The trailing path segment of a refname, parsed back as an [`ObjectId`] —
/// every note ref's identity, binding-keyed or genesis-keyed alike.
fn leaf_id(refname: &str) -> Result<ObjectId> {
    let leaf = refname.rsplit('/').next().unwrap_or_default();
    ObjectId::from_hex(leaf.as_bytes())
        .map_err(|error| Error::Object(format!("ref {refname:?} has a non-oid leaf: {error}")))
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
