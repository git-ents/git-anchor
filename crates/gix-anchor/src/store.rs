//! [`Store`]: notes — arbitrary content attached to a [`Binding`]'s
//! target — persisted as Git refs and commits, git-notes style.
//!
//! One ref per (target, binding-identity) pair, at
//! `refs/anchors/<target-hex>/<binding-oid-hex>`: attaching again to the
//! same binding commits a new version forward onto the same ref, so editing
//! a note keeps its full history rather than overwriting it. The identity
//! oid is the binding's own serialized tree oid — deterministic and
//! content-addressed on the binding, not on the attached body — so the same
//! binding always resolves to the same ref regardless of which process
//! attached it first.

use std::time::Duration;

use facet::Facet;
use facet_git_tree::RawTree;
use gix::ObjectId;

use crate::binding::Binding;
use crate::error::{Error, Result};
use crate::refname::check_hex_component;

/// Where note refs live: `refs/anchors/<target-hex>/<binding-oid-hex>`.
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

/// The document committed at a note's ref: an arbitrary attached `body` plus
/// the [`Binding`] it is attached to, embedded by tree id
/// (`anchor.retention`) so the anchor's own content and context blobs stay
/// reachable through the note's own tree.
#[derive(Facet)]
struct Note {
    body: Vec<u8>,
    binding: RawTree,
}

/// A content-addressed store of notes over a `gix` repository, git-notes
/// style: one note per [`Binding`] identity, editable with full history.
pub struct Store<'r> {
    repo: &'r gix::Repository,
}

/// A note read back from the [`Store`].
pub struct StoredNote {
    /// The note's identity oid — the binding's own serialized tree oid, and
    /// the ref-path leaf.
    pub id: ObjectId,
    /// [`Binding::target`] — the ref-path grouping key.
    pub target: ObjectId,
    /// The binding this note is attached to, recovered from storage.
    pub binding: Binding,
    /// The arbitrary content attached to `binding`.
    pub body: Vec<u8>,
    /// The latest version's commit summary (first line of the message).
    pub message: String,
}

impl<'r> Store<'r> {
    /// Open a store over `repo` with the default `refs/anchors` prefix.
    #[must_use]
    pub fn open(repo: &'r gix::Repository) -> Store<'r> {
        Store { repo }
    }

    /// Attach `body` to the object `binding` names, git-notes style: one
    /// note per (target, binding-identity), editable with history.
    ///
    /// The identity oid — `binding`'s own serialized tree oid — is
    /// deterministic and content-addressed on the binding alone, never on
    /// `body`, so re-attaching to the same binding commits a new version
    /// forward onto the same ref (`refs/anchors/<target>/<id>`) instead of
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
        let target = binding.target();
        let id = binding.serialize_into(&self.repo.objects)?;
        check_hex_component("target", &target.to_string())?;
        check_hex_component("id", &id.to_string())?;

        let note = Note {
            body: body.to_vec(),
            binding: RawTree::new(id),
        };
        let tree = facet_git_tree::serialize_into(&note, &self.repo.objects)?;

        let refname = anchor_ref(target, id);
        let default_summary = format!("anchor {target}");
        let summary = message.unwrap_or(&default_summary);
        self.commit_forward(&refname, summary, tree)?;
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
            Some(target) => format!("{ANCHOR_PREFIX}/{target}/"),
            None => format!("{ANCHOR_PREFIX}/"),
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
    fn read_note(&self, refname: &str) -> Result<Option<StoredNote>> {
        let Some(tip) = self.tip(refname)? else {
            return Ok(None);
        };
        let commit = self.repo.find_commit(tip).map_err(Error::git)?;
        let tree = commit.tree_id().map_err(Error::git)?.detach();
        let note: Note = facet_git_tree::deserialize(&tree, &self.repo.objects)?;
        let id = note.binding.oid();
        let binding = Binding::deserialize(&id, &self.repo.objects)?;
        let target = binding.target();
        let message = gix_object::commit::MessageRef::from_bytes(commit.message_raw_sloppy())
            .summary()
            .to_string();
        Ok(Some(StoredNote {
            id,
            target,
            binding,
            body: note.body,
            message,
        }))
    }

    /// The full refname of the note with identity `id`, scanning every ref
    /// under [`ANCHOR_PREFIX`] for a matching leaf segment. `None` when no
    /// such note exists.
    fn find_ref(&self, id: ObjectId) -> Result<Option<String>> {
        let leaf = id.to_string();
        for refname in self.refs_under(&format!("{ANCHOR_PREFIX}/"))? {
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

/// `refs/anchors/<target>/<id>`.
fn anchor_ref(target: ObjectId, id: ObjectId) -> String {
    format!("{ANCHOR_PREFIX}/{target}/{id}")
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
