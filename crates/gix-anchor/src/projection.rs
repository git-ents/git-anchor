//! Read-time projection of an [`Anchor`](crate::Anchor) onto another
//! commit: an exact tree diff when the anchor's own commit still exists,
//! degrading to fuzzy context matching once it is gone.
//!
//! Spec coverage: `anchor.projection`, `anchor.fuzzy-fallback`.
//!
//! Projection is a two-point tree diff, not a history walk: [`project_exact`]
//! compares the anchor commit's tree directly against the target commit's
//! tree, so it works whether the target is a descendant, an ancestor, or
//! unrelated history. Blame answers the backwards question (which commit
//! introduced a line); the forward question asked here needs only the diff.

use gix::ObjectId;
use gix::bstr::ByteSlice as _;
use gix::diff::blob::{Algorithm, Diff, InternedInput};
use gix::diff::tree_with_rewrites::Change;

use crate::anchor::{Anchor, CONTEXT_MARGIN, LineRange};
use crate::error::{Error, Result};
use crate::util::{commit_at, read_blob, resolve_commit};

/// Where an [`Anchor`] sits on a target commit, as computed by [`project`].
///
/// # Examples
///
/// ```
/// use gix_anchor::Projection;
///
/// let outcome = Projection::Outdated { path: "src/lib.rs".to_owned() };
/// assert!(matches!(outcome, Projection::Outdated { .. }));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Projection {
    /// The target tree holds the anchor's exact blob at its exact path; the
    /// anchor applies unchanged.
    Current,
    /// The file moved and/or its content shifted, but the anchored region
    /// itself is intact — the anchor now applies at `path` and `lines`.
    Relocated {
        /// The anchored file's path in the target tree.
        path: String,
        /// The anchored lines mapped into the target blob, or `None` for a
        /// whole-file anchor.
        lines: Option<LineRange>,
    },
    /// The file survives at `path` but the anchored lines were edited (or
    /// the entry is no longer a regular file); the anchor no longer maps
    /// cleanly.
    Outdated {
        /// The anchored file's path in the target tree.
        path: String,
    },
    /// The anchored file does not exist in the target tree.
    Deleted,
}

impl Projection {
    /// The outcome's canonical lowercase keyword -- the porcelain grammar's
    /// own vocabulary (`current`, `relocated`, `outdated`, `deleted`), shared
    /// by every surface that names an outcome without narrating its payload.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Relocated { .. } => "relocated",
            Self::Outdated { .. } => "outdated",
            Self::Deleted => "deleted",
        }
    }

    /// Decompose `self` into its two independent axes: whether the anchored
    /// region *moved* ([`Position`]) and whether it was *edited*
    /// ([`Content`]). [`Projection`] conflates both into one four-valued
    /// outcome; a policy consumer deciding, say, whether a review carries
    /// forward needs to ask the two questions separately -- a span that
    /// moved but is textually intact is a very different case from one that
    /// stayed put but was edited, and a flat `Projection` cannot distinguish
    /// them from its label alone.
    ///
    /// Takes `anchor`: [`Self::Relocated`] and [`Self::Outdated`] each carry
    /// only the *destination* path, and telling a rename (a new [`Position`])
    /// apart from an in-place edit (the same one) means comparing it against
    /// the anchor's own `identity.path`. That comparison is exact,
    /// not an approximation -- [`project_exact`]'s tree diff, with rename
    /// tracking, already decided the destination path precisely when it
    /// built `self`, so this recovers that decision rather than guessing at
    /// it. Callers should use this method rather than comparing paths
    /// themselves.
    #[must_use]
    pub fn axes(&self, anchor: &Anchor) -> (Position, Content) {
        match self {
            Self::Current => (Position::Same, Content::Intact),
            Self::Relocated { path, .. } => {
                (Position::of(path, &anchor.identity.path), Content::Intact)
            }
            Self::Outdated { path } => (Position::of(path, &anchor.identity.path), Content::Edited),
            Self::Deleted => (Position::Lost, Content::None),
        }
    }
}

/// Whether an anchored region sits where it was captured -- the position
/// half of [`Projection::axes`]'s decomposition; [`Content`] is the other
/// half, and the two vary independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    /// The anchor's file sits at the same path it was captured against.
    Same,
    /// The anchor's file sits at a different path.
    Moved,
    /// The anchor's file no longer exists in the target tree.
    Lost,
}

impl Position {
    /// `Same` when `path` (a projection's destination) equals `anchor_path`
    /// (the anchor's own path), `Moved` otherwise.
    fn of(path: &str, anchor_path: &str) -> Self {
        if path == anchor_path {
            Self::Same
        } else {
            Self::Moved
        }
    }

    /// The axis's canonical lowercase keyword -- part of the same porcelain
    /// vocabulary [`Projection::label`] documents (`same`, `moved`, `lost`).
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Same => "same",
            Self::Moved => "moved",
            Self::Lost => "lost",
        }
    }
}

/// Whether an anchored region's own text is unchanged -- the content half of
/// [`Projection::axes`]'s decomposition; [`Position`] is the other half, and
/// the two vary independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    /// The anchored lines (or, for a whole-file anchor, the whole file) are
    /// byte-identical to what [`crate::capture`] recorded.
    Intact,
    /// The file survives but the anchored region itself was edited.
    Edited,
    /// There is no content to speak of. Legal only paired with
    /// [`Position::Lost`] -- every other [`Projection`] outcome names a
    /// surviving file, edited or not.
    None,
}

impl Content {
    /// The axis's canonical lowercase keyword -- part of the same porcelain
    /// vocabulary [`Projection::label`] documents (`intact`, `edited`, `none`).
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Intact => "intact",
            Self::Edited => "edited",
            Self::None => "none",
        }
    }
}

/// Project `anchor` onto `target` (a revision in `repo`), degrading to
/// [`project_from_context`] once `anchor`'s own commit has been garbage
/// collected (`anchor.fuzzy-fallback`) — the one entry point most callers
/// need; [`project_exact`] and [`project_from_context`] are exposed
/// separately for callers that need to distinguish an exact projection from
/// an approximate one.
///
/// Never mutates `anchor`: every outcome, including [`Projection::Outdated`]
/// and [`Projection::Deleted`], is a fresh [`Projection`] value, and the
/// anchor itself remains displayable regardless of the outcome
/// (`anchor.fuzzy-fallback`).
///
/// # Unsuitable for caching or gating
///
/// The fallback to [`project_from_context`] triggers on whether `anchor`'s
/// own commit is still reachable in the object database -- ambient
/// garbage-collection state that is neither content-addressed nor a ref, so
/// it cannot be captured in any cache key and does not correspond to any
/// input a gate could pin. Concretely: a renamed file reports
/// [`Projection::Relocated`] through this function before a `git gc` runs
/// and [`Projection::Deleted`] after one, with `anchor` and `target`
/// unchanged, because [`project_from_context`] does no rename tracking at
/// all (its own doc comment says so). Any caller that caches `project`'s
/// result, or gates a decision on it, is therefore caching or gating on
/// whether maintenance happened to run. Call [`project_exact`] or
/// [`project_from_context`] explicitly instead, so the choice of heuristic
/// -- and its version, [`PROJECTION_HEURISTIC_VERSION`] -- is itself part of
/// what the caller committed to.
///
/// # Examples
///
/// ```
/// # let dir = tempfile::tempdir().expect("tempdir");
/// # std::process::Command::new("git").arg("init").arg("-q").arg(dir.path()).status().unwrap();
/// # std::fs::write(dir.path().join("file.txt"), "a\nb\nc\n").unwrap();
/// # std::process::Command::new("git").arg("-C").arg(dir.path()).args(["add", "-A"]).status().unwrap();
/// # std::process::Command::new("git").arg("-C").arg(dir.path())
/// #     .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "-m", "one"])
/// #     .status().unwrap();
/// let repo = gix::open(dir.path()).expect("open");
/// let anchor = gix_anchor::capture(&repo, "HEAD", "file.txt", None).expect("capture");
/// assert_eq!(gix_anchor::project(&repo, &anchor, "HEAD").unwrap(), gix_anchor::Projection::Current);
/// ```
pub fn project(repo: &gix::Repository, anchor: &Anchor, target: &str) -> Result<Projection> {
    match project_exact(repo, anchor, target) {
        Err(Error::AnchorCommitMissing(_)) => project_from_context(repo, anchor, target),
        other => other,
    }
}

/// Version of [`project_exact`]'s rename-detection and hunk-mapping
/// heuristics. Bump this on **any** behavior change to either — not just a
/// deliberate tuning change, but also a `gix` upgrade that changes what its
/// diff or rewrite-tracking produces for the same inputs. Downstream callers
/// that cache a projection (or a value derived from one) fold this into
/// their cache key alongside the data inputs; a `Projection` that starts
/// coming back different for the same `(Anchor, target)` pair without this
/// bumping poisons every cache entry computed under the old behavior, since
/// nothing else in the inputs changed to invalidate them.
pub const PROJECTION_HEURISTIC_VERSION: u32 = 1;

/// Project `anchor` onto `target` by diffing `anchor`'s own commit tree
/// against `target`'s, with rename tracking, and mapping the line range
/// through the blob diff's hunks — shifted past edits that land entirely
/// outside it, [`Projection::Outdated`] when an edit touches it.
///
/// Fails with [`Error::AnchorCommitMissing`] when `anchor`'s commit no
/// longer exists (it is retained on a best-effort basis only,
/// `anchor.retention`); [`project`] catches exactly this and retries with
/// [`project_from_context`], which needs no commit at all.
pub fn project_exact(repo: &gix::Repository, anchor: &Anchor, target: &str) -> Result<Projection> {
    let target_commit = resolve_commit(repo, target)?;
    let target_tree = target_commit
        .tree()
        .map_err(|error| Error::Object(error.to_string()))?;
    project_exact_onto(repo, anchor, &target_tree)
}

/// Project `anchors` onto `target` in a single pass, resolving `target`'s
/// tree once for the whole batch rather than once per anchor — the
/// difference between linear and quadratic when a caller (a fixpoint
/// evaluator revisiting every anchor at one revision, for instance) needs
/// [`project_exact`] for a large anchor set at the same `target`.
///
/// Returns one [`Result`] per anchor, in the same order as `anchors`, each
/// identical to what calling [`project_exact`] on that anchor alone would
/// have returned; only the shared, up-front `target` resolution differs.
pub fn project_many(
    repo: &gix::Repository,
    anchors: &[Anchor],
    target: &str,
) -> Result<Vec<Result<Projection>>> {
    let target_commit = resolve_commit(repo, target)?;
    let target_tree = target_commit
        .tree()
        .map_err(|error| Error::Object(error.to_string()))?;
    Ok(anchors
        .iter()
        .map(|anchor| project_exact_onto(repo, anchor, &target_tree))
        .collect())
}

/// Every path in `target`'s tree [`project_exact`] could equally justify as
/// `anchor`'s destination, for the case [`project_exact`] itself cannot
/// represent: `anchor`'s content surviving intact at more than one path (a
/// duplicate or an untracked copy). Multiplicity *is* the ambiguity — there
/// is deliberately no `Ambiguous` variant, so a caller that cares checks
/// `project_candidates(..).len() > 1` rather than being handed a status that
/// already discarded which candidates existed. A result of length one always
/// agrees exactly with [`project_exact`].
///
/// Only [`Projection::Current`] and [`Projection::Relocated`] can yield more
/// than one candidate — content survives intact in both, which is the only
/// circumstance under which the same bytes can legitimately sit at more than
/// one path. [`Projection::Outdated`] and [`Projection::Deleted`] each name
/// exactly one destination (or none), so both pass through as the sole
/// element of a one-long vector, identical to [`project_exact`].
pub fn project_candidates(
    repo: &gix::Repository,
    anchor: &Anchor,
    target: &str,
) -> Result<Vec<Projection>> {
    let target_commit = resolve_commit(repo, target)?;
    let target_tree = target_commit
        .tree()
        .map_err(|error| Error::Object(error.to_string()))?;
    let primary = project_exact_onto(repo, anchor, &target_tree)?;

    let (needle, lines) = match &primary {
        Projection::Current => (ObjectId::from(anchor.hints.blob), anchor.identity.lines),
        Projection::Relocated { path, lines } => {
            let id = target_tree
                .lookup_entry_by_path(path)
                .map_err(|error| Error::Object(error.to_string()))?
                .ok_or_else(|| Error::MissingPath {
                    commit: target_commit.id().detach(),
                    path: path.clone(),
                })?
                .object_id();
            (id, *lines)
        }
        Projection::Outdated { .. } | Projection::Deleted => return Ok(vec![primary]),
    };

    let anchor_blob = ObjectId::from(anchor.hints.blob);
    let mut candidates: Vec<(String, Projection)> = target_tree
        .traverse()
        .breadthfirst
        .files()
        .map_err(|error| Error::Object(error.to_string()))?
        .into_iter()
        .filter(|entry| entry.mode.is_blob() && entry.oid == needle)
        .map(|entry| {
            let path = entry.filepath.to_str_lossy().into_owned();
            let projection = if path == anchor.identity.path && entry.oid == anchor_blob {
                Projection::Current
            } else {
                Projection::Relocated {
                    path: path.clone(),
                    lines,
                }
            };
            (path, projection)
        })
        .collect();
    candidates.sort_by(|(a, _), (b, _)| a.cmp(b));
    Ok(candidates
        .into_iter()
        .map(|(_, projection)| projection)
        .collect())
}

/// [`project_exact`]'s implementation, taking an already-resolved
/// `target_tree` so [`project_many`] and [`project_candidates`] can share
/// one tree resolution across a batch or a follow-up scan instead of each
/// re-resolving `target` from scratch.
fn project_exact_onto(
    repo: &gix::Repository,
    anchor: &Anchor,
    target_tree: &gix::Tree<'_>,
) -> Result<Projection> {
    let anchor_blob = ObjectId::from(anchor.hints.blob);
    let anchor_commit_id = ObjectId::from(anchor.identity.genesis);

    if let Some(entry) = target_tree
        .lookup_entry_by_path(&anchor.identity.path)
        .map_err(|error| Error::Object(error.to_string()))?
        && entry.mode().is_blob()
        && entry.object_id() == anchor_blob
    {
        return Ok(Projection::Current);
    }

    if !repo.has_object(anchor_commit_id) {
        return Err(Error::AnchorCommitMissing(anchor_commit_id));
    }
    let anchor_commit = commit_at(repo, anchor_commit_id)?;
    let anchor_tree = anchor_commit
        .tree()
        .map_err(|error| Error::Object(error.to_string()))?;
    // Rename tracking is pinned to git's defaults (50% similarity, no
    // copies) rather than read from repository configuration, so a
    // projection is the same answer everywhere the repository is checked
    // out.
    let options = gix::diff::Options::default().with_rewrites(Some(gix::diff::Rewrites::default()));
    let changes = repo
        .diff_tree_to_tree(Some(&anchor_tree), Some(target_tree), options)
        .map_err(|error| Error::Diff(error.to_string()))?;

    // Find where the anchored path went: its old-side location is
    // `location` for a deletion or modification and `source_location` for a
    // rename.
    let mut destination: Option<(String, ObjectId, bool)> = None;
    for change in changes {
        match change {
            Change::Deletion { location, .. }
                if location.as_bytes() == anchor.identity.path.as_bytes() =>
            {
                return Ok(Projection::Deleted);
            }
            Change::Modification {
                location,
                id,
                entry_mode,
                ..
            } if location.as_bytes() == anchor.identity.path.as_bytes() => {
                destination = Some((anchor.identity.path.clone(), id, entry_mode.is_blob()));
                break;
            }
            Change::Rewrite {
                source_location,
                location,
                id,
                entry_mode,
                copy: false,
                ..
            } if source_location.as_bytes() == anchor.identity.path.as_bytes() => {
                destination = Some((
                    location.to_str_lossy().into_owned(),
                    id,
                    entry_mode.is_blob(),
                ));
                break;
            }
            _ => {}
        }
    }
    let Some((path, blob, is_blob)) = destination else {
        // The diff never touched the path, yet the fast path did not
        // match: the anchor's blob is not what its own commit holds there,
        // so the anchor itself is broken.
        return Err(Error::MissingPath {
            commit: anchor_commit_id,
            path: anchor.identity.path.clone(),
        });
    };
    if !is_blob {
        return Ok(Projection::Outdated { path });
    }
    if blob == anchor_blob {
        // A pure rename: the content is byte-identical, so every line is
        // exactly where it was.
        return Ok(Projection::Relocated {
            path,
            lines: anchor.identity.lines,
        });
    }
    let lines = match anchor.identity.lines {
        None => None,
        Some(range) => {
            let new = read_blob(repo, blob)?;
            match map_range(&anchor.hints.content, &new, range) {
                Some(mapped) => Some(mapped),
                None => return Ok(Projection::Outdated { path }),
            }
        }
    };
    Ok(Projection::Relocated { path, lines })
}

/// Project `anchor` onto `target` by fuzzy-matching `anchor`'s retained
/// `hints.context` (`anchor.retention`) against `target`'s version of
/// `identity.path`, for use once `anchor`'s commit no longer exists and
/// [`project_exact`] can no longer diff against its tree.
///
/// Looks up `identity.path` in `target`'s tree directly (no rename tracking
/// is possible without the anchor commit's tree, so a genuine rename reports
/// [`Projection::Deleted`] here, same as a real deletion); a whole-file
/// anchor (`identity.lines` is `None`) survives any edit at that path, same
/// as [`project_exact`]. For a line-range anchor, every contiguous window of
/// the target file's lines the same length as `context` is scored by how
/// many lines match `context` exactly; the best-scoring window (at least
/// half its lines matching) is accepted and the anchored sub-range is mapped
/// back through the same margin [`crate::capture`] used to build `context`.
/// No match clearing that bar reports [`Projection::Outdated`], the same as
/// an unrecoverable edit would under [`project_exact`].
pub fn project_from_context(
    repo: &gix::Repository,
    anchor: &Anchor,
    target: &str,
) -> Result<Projection> {
    let target_commit = resolve_commit(repo, target)?;
    let target_tree = target_commit
        .tree()
        .map_err(|error| Error::Object(error.to_string()))?;
    let Some(entry) = target_tree
        .lookup_entry_by_path(&anchor.identity.path)
        .map_err(|error| Error::Object(error.to_string()))?
    else {
        return Ok(Projection::Deleted);
    };
    if !entry.mode().is_blob() {
        return Ok(Projection::Outdated {
            path: anchor.identity.path.clone(),
        });
    }
    let Some(range) = anchor.identity.lines else {
        return Ok(Projection::Relocated {
            path: anchor.identity.path.clone(),
            lines: None,
        });
    };

    let data = read_blob(repo, entry.object_id())?;
    let target_lines: Vec<&[u8]> = data.lines_with_terminator().collect();
    let context_lines: Vec<&[u8]> = anchor.hints.context.lines_with_terminator().collect();
    let window = context_lines.len();
    if window == 0 || window > target_lines.len() {
        return Ok(Projection::Outdated {
            path: anchor.identity.path.clone(),
        });
    }

    let mut best: Option<(usize, usize)> = None;
    for (start, slice) in target_lines.windows(window).enumerate() {
        let score = slice
            .iter()
            .zip(context_lines.iter())
            .filter(|(have, want)| have == want)
            .count();
        if best.is_none_or(|(_start, best_score)| score > best_score) {
            best = Some((start, score));
        }
    }
    // Require at least half the window's lines to match exactly, so an
    // unrelated coincidence of blank or near-empty lines is not mistaken
    // for the anchored region having relocated there.
    let Some((start, _score)) = best.filter(|(_start, score)| {
        score
            .checked_mul(2)
            .is_some_and(|doubled| doubled >= window)
    }) else {
        return Ok(Projection::Outdated {
            path: anchor.identity.path.clone(),
        });
    };

    let margin_before = CONTEXT_MARGIN.min(range.start.saturating_sub(1));
    let range_len = range.end.saturating_sub(range.start).saturating_add(1);
    let Ok(start) = u64::try_from(start) else {
        return Ok(Projection::Outdated {
            path: anchor.identity.path.clone(),
        });
    };
    let mapped_start = start.saturating_add(margin_before).saturating_add(1);
    let mapped_end = mapped_start.saturating_add(range_len).saturating_sub(1);
    Ok(Projection::Relocated {
        path: anchor.identity.path.clone(),
        lines: Some(LineRange {
            start: mapped_start,
            end: mapped_end,
        }),
    })
}

/// Project `anchor` onto the working tree (`anchor.working-tree`): diff the
/// anchored blob — always available, it is embedded (`anchor.retention`) —
/// against the path's current on-disk bytes, or against `buffer` standing
/// in for them (`lens.working-tree`'s unsaved-editor-buffer case), and
/// report the same four outcomes as [`project`].
///
/// There is no commit on the target side to diff trees against, so rename
/// following degrades exactly as [`project_from_context`]'s does
/// (`anchor.working-tree`): only `identity.path` itself is consulted, and a
/// file that moved on disk reports [`Projection::Deleted`] the same as a
/// removed one. The line mapping itself never degrades: the embedded
/// content makes the exact blob diff [`project_exact`] uses available even
/// when the anchor's own commit is long gone.
///
/// # Examples
///
/// ```
/// # let dir = tempfile::tempdir().expect("tempdir");
/// # std::process::Command::new("git").arg("init").arg("-q").arg(dir.path()).status().unwrap();
/// # std::fs::write(dir.path().join("file.txt"), "a\nb\nc\n").unwrap();
/// # std::process::Command::new("git").arg("-C").arg(dir.path()).args(["add", "-A"]).status().unwrap();
/// # std::process::Command::new("git").arg("-C").arg(dir.path())
/// #     .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "-m", "one"])
/// #     .status().unwrap();
/// use gix_anchor::{LineRange, Projection};
///
/// let repo = gix::open(dir.path()).expect("open");
/// let anchor = gix_anchor::capture(&repo, "HEAD", "file.txt", Some(LineRange { start: 2, end: 2 }))
///     .expect("capture");
///
/// // Dirty the working tree above the anchored line: the anchor relocates,
/// // no commit involved on the target side.
/// std::fs::write(dir.path().join("file.txt"), "inserted\na\nb\nc\n").unwrap();
/// assert_eq!(
///     gix_anchor::project_worktree(&repo, &anchor, None).expect("project"),
///     Projection::Relocated {
///         path: "file.txt".to_owned(),
///         lines: Some(LineRange { start: 3, end: 3 }),
///     }
/// );
///
/// // A caller-supplied buffer stands in for the on-disk bytes.
/// assert_eq!(
///     gix_anchor::project_worktree(&repo, &anchor, Some(b"a\nb\nc\n")).expect("project"),
///     Projection::Current
/// );
/// ```
pub fn project_worktree(
    repo: &gix::Repository,
    anchor: &Anchor,
    buffer: Option<&[u8]>,
) -> Result<Projection> {
    let outdated = || {
        Ok(Projection::Outdated {
            path: anchor.identity.path.clone(),
        })
    };
    let owned;
    let bytes: &[u8] = match buffer {
        Some(bytes) => bytes,
        None => {
            let workdir = repo.workdir().ok_or(Error::NoWorkingTree)?;
            let file = workdir.join(&anchor.identity.path);
            let Ok(metadata) = std::fs::metadata(&file) else {
                return Ok(Projection::Deleted);
            };
            if !metadata.is_file() {
                // The entry is no longer a regular file — the same
                // taxonomy row `project_exact` reports for a mode change.
                return outdated();
            }
            owned = std::fs::read(&file).map_err(|error| Error::Object(error.to_string()))?;
            &owned
        }
    };
    if bytes == anchor.hints.content.as_slice() {
        // Byte equality is blob-id equality: the exact anchored blob still
        // sits at the anchored path.
        return Ok(Projection::Current);
    }
    let Some(range) = anchor.identity.lines else {
        return Ok(Projection::Relocated {
            path: anchor.identity.path.clone(),
            lines: None,
        });
    };
    match map_range(&anchor.hints.content, bytes, range) {
        Some(lines) => Ok(Projection::Relocated {
            path: anchor.identity.path.clone(),
            lines: Some(lines),
        }),
        None => outdated(),
    }
}

/// Map the 1-based inclusive `range` from `old`'s lines to `new`'s by
/// walking the diff's hunks in order: a hunk entirely above the range
/// shifts it by the hunk's growth, a hunk entirely below is ignored, and any
/// hunk touching the range — including an insertion strictly inside it —
/// means the anchored region itself changed, reported as `None` (outdated)
/// rather than guessed at.
fn map_range(old: &[u8], new: &[u8], range: LineRange) -> Option<LineRange> {
    // Work in 0-based half-open line coordinates, as the hunks do.
    // Everything stays unsigned: the shift is tallied as lines added and
    // lines removed above the range, and any overflow is an honest `None`
    // (outdated) via the checked arithmetic rather than a saturated wrong
    // answer.
    let start = range.start.checked_sub(1)?;
    let end = range.end;
    if end <= start {
        return None;
    }
    let input = InternedInput::new(old, new);
    if end > u64::try_from(input.before.len()).ok()? {
        return None;
    }
    let diff = Diff::compute(Algorithm::Histogram, &input);
    let mut added: u64 = 0;
    let mut removed: u64 = 0;
    for hunk in diff.hunks() {
        let before_start = u64::from(hunk.before.start);
        let before_end = u64::from(hunk.before.end);
        if before_end <= start {
            removed = removed.checked_add(before_end.checked_sub(before_start)?)?;
            added = added
                .checked_add(u64::from(hunk.after.end).checked_sub(u64::from(hunk.after.start))?)?;
        } else if before_start >= end {
            break;
        } else {
            return None;
        }
    }
    let map = |line: u64| line.checked_add(added)?.checked_sub(removed);
    Some(LineRange {
        start: map(start)?.checked_add(1)?,
        end: map(end)?,
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::arithmetic_side_effects,
        reason = "unit test; property inputs are bounded well below overflow"
    )]

    use rstest::rstest;

    use super::*;
    use crate::anchor::capture;
    use crate::fixture::{commit_all, numbered, repo};

    fn range(start: u64, end: u64) -> Option<LineRange> {
        Some(LineRange { start, end })
    }

    #[rstest]
    #[case::current(Projection::Current, "current")]
    #[case::relocated(Projection::Relocated { path: "f".to_owned(), lines: None }, "relocated")]
    #[case::outdated(Projection::Outdated { path: "f".to_owned() }, "outdated")]
    #[case::deleted(Projection::Deleted, "deleted")]
    fn label_is_the_porcelain_keyword(#[case] projection: Projection, #[case] expected: &str) {
        assert_eq!(projection.label(), expected);
    }

    #[rstest]
    #[case::same(Position::Same, "same")]
    #[case::moved(Position::Moved, "moved")]
    #[case::lost(Position::Lost, "lost")]
    fn position_label_is_the_axis_keyword(#[case] position: Position, #[case] expected: &str) {
        assert_eq!(position.label(), expected);
    }

    #[rstest]
    #[case::intact(Content::Intact, "intact")]
    #[case::edited(Content::Edited, "edited")]
    #[case::none(Content::None, "none")]
    fn content_label_is_the_axis_keyword(#[case] content: Content, #[case] expected: &str) {
        assert_eq!(content.label(), expected);
    }

    /// [`Projection::axes`] over all four variants, including both the
    /// same-path and moved-path forms of `Relocated` and `Outdated` -- the
    /// two rows the position axis actually has to decide between.
    #[rstest]
    #[case::current(Projection::Current, Position::Same, Content::Intact)]
    #[case::relocated_same_path(
        Projection::Relocated { path: "file.txt".to_owned(), lines: None },
        Position::Same,
        Content::Intact
    )]
    #[case::relocated_moved_path(
        Projection::Relocated { path: "moved.txt".to_owned(), lines: None },
        Position::Moved,
        Content::Intact
    )]
    #[case::outdated_same_path(
        Projection::Outdated { path: "file.txt".to_owned() },
        Position::Same,
        Content::Edited
    )]
    #[case::outdated_moved_path(
        Projection::Outdated { path: "moved.txt".to_owned() },
        Position::Moved,
        Content::Edited
    )]
    #[case::deleted(Projection::Deleted, Position::Lost, Content::None)]
    fn axes_decompose_position_and_content(
        #[case] projection: Projection,
        #[case] expected_position: Position,
        #[case] expected_content: Content,
    ) {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(&git_repo, "HEAD", "file.txt", None).unwrap();

        assert_eq!(
            projection.axes(&anchor),
            (expected_position, expected_content)
        );
    }

    /// One post-capture edit per taxonomy row of
    /// [`projection_reports_the_spec_outcomes`].
    #[derive(Debug, Clone, Copy)]
    enum Mutation {
        TouchOtherFile,
        PrependTwoLines,
        EditLineFive,
        Rename,
        RenameAndPrependOneLine,
        Delete,
    }

    impl Mutation {
        fn apply(self, dir: &std::path::Path) {
            let file = dir.join("file.txt");
            match self {
                Self::TouchOtherFile => {
                    std::fs::write(dir.join("other.txt"), "unrelated\n").unwrap();
                }
                Self::PrependTwoLines => {
                    std::fs::write(&file, format!("added a\nadded b\n{}", numbered(1..=10)))
                        .unwrap();
                }
                Self::EditLineFive => {
                    let edited = numbered(1..=10).replace("line 5\n", "line five\n");
                    std::fs::write(&file, edited).unwrap();
                }
                Self::Rename => {
                    std::fs::rename(&file, dir.join("moved.txt")).unwrap();
                }
                Self::RenameAndPrependOneLine => {
                    std::fs::remove_file(&file).unwrap();
                    std::fs::write(
                        dir.join("moved.txt"),
                        format!("added a\n{}", numbered(1..=10)),
                    )
                    .unwrap();
                }
                Self::Delete => {
                    std::fs::remove_file(&file).unwrap();
                    std::fs::write(dir.join("unrelated.txt"), "different content\n").unwrap();
                }
            }
        }
    }

    /// `anchor.projection`'s outcome taxonomy, enumerated over the
    /// scenarios that select each outcome: unchanged (current), an edit
    /// above the range (relocated: shifted), an edit inside the range
    /// (outdated), a pure rename (relocated: same lines), a rename with an
    /// edit above (relocated: new path and shifted lines), a deletion
    /// (deleted), and a whole-file anchor surviving a modification
    /// (relocated: no lines).
    #[rstest]
    #[case::unchanged_is_current(Mutation::TouchOtherFile, range(3, 4), Projection::Current)]
    #[case::edit_above_shifts(
        Mutation::PrependTwoLines,
        range(5, 6),
        Projection::Relocated { path: "file.txt".to_owned(), lines: range(7, 8) }
    )]
    #[case::edit_inside_outdates(
        Mutation::EditLineFive,
        range(5, 6),
        Projection::Outdated { path: "file.txt".to_owned() }
    )]
    #[case::pure_rename_relocates(
        Mutation::Rename,
        range(3, 4),
        Projection::Relocated { path: "moved.txt".to_owned(), lines: range(3, 4) }
    )]
    #[case::rename_with_edit_above(
        Mutation::RenameAndPrependOneLine,
        range(5, 6),
        Projection::Relocated { path: "moved.txt".to_owned(), lines: range(6, 7) }
    )]
    #[case::deletion_is_deleted(Mutation::Delete, range(3, 4), Projection::Deleted)]
    #[case::whole_file_survives_an_edit(
        Mutation::EditLineFive,
        None,
        Projection::Relocated { path: "file.txt".to_owned(), lines: None }
    )]
    fn projection_reports_the_spec_outcomes(
        #[case] mutation: Mutation,
        #[case] lines: Option<LineRange>,
        #[case] expected: Projection,
    ) {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(&git_repo, "HEAD", "file.txt", lines).unwrap();

        mutation.apply(dir.path());
        commit_all(dir.path(), "two");

        // Re-open: the first handle predates commit two.
        let git_repo = gix::open(dir.path()).unwrap();
        assert_eq!(project_exact(&git_repo, &anchor, "HEAD").unwrap(), expected);
        // The umbrella entry point gives the identical answer while the
        // anchor commit exists.
        assert_eq!(project(&git_repo, &anchor, "HEAD").unwrap(), expected);
    }

    /// [`project_candidates`] over every taxonomy row of
    /// [`projection_reports_the_spec_outcomes`] where nothing duplicates the
    /// destination content: exactly one candidate, identical to
    /// [`project_exact`]'s single answer.
    #[rstest]
    #[case::unchanged_is_current(Mutation::TouchOtherFile)]
    #[case::edit_above_shifts(Mutation::PrependTwoLines)]
    #[case::edit_inside_outdates(Mutation::EditLineFive)]
    #[case::pure_rename_relocates(Mutation::Rename)]
    #[case::rename_with_edit_above(Mutation::RenameAndPrependOneLine)]
    #[case::deletion_is_deleted(Mutation::Delete)]
    fn project_candidates_with_no_duplicate_agrees_with_project_exact(#[case] mutation: Mutation) {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(&git_repo, "HEAD", "file.txt", range(5, 6)).unwrap();

        mutation.apply(dir.path());
        commit_all(dir.path(), "two");
        let git_repo = gix::open(dir.path()).unwrap();

        let expected = project_exact(&git_repo, &anchor, "HEAD").unwrap();
        assert_eq!(
            project_candidates(&git_repo, &anchor, "HEAD").unwrap(),
            vec![expected]
        );
    }

    /// A blob whose exact bytes appear at two paths in the target tree --
    /// [`project_exact`] can only report the one destination its tree diff
    /// found, but [`project_candidates`] reports both, which is what makes
    /// ambiguity representable at all (`git-query` derives it as a join over
    /// candidates rather than needing an `Ambiguous` status here).
    #[test]
    fn project_candidates_reports_every_path_holding_the_same_content() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(&git_repo, "HEAD", "file.txt", range(3, 4)).unwrap();

        // Duplicate the file's exact bytes at a second path; the original
        // is left untouched, so this is an untracked copy rather than a
        // rename `project_exact`'s diff would already have found.
        std::fs::write(dir.path().join("copy.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "two");
        let git_repo = gix::open(dir.path()).unwrap();

        assert_eq!(
            project_candidates(&git_repo, &anchor, "HEAD").unwrap(),
            vec![
                Projection::Relocated {
                    path: "copy.txt".to_owned(),
                    lines: range(3, 4),
                },
                Projection::Current,
            ]
        );
        // project_exact, unable to represent the multiplicity, reports only
        // the untouched original.
        assert_eq!(
            project_exact(&git_repo, &anchor, "HEAD").unwrap(),
            Projection::Current
        );
    }

    #[test]
    fn projection_works_backwards_onto_an_ancestor() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let old = git_repo.head_id().unwrap().detach().to_string();

        let edited = format!("added a\n{}", numbered(1..=10));
        std::fs::write(dir.path().join("file.txt"), edited).unwrap();
        commit_all(dir.path(), "two");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(&git_repo, "HEAD", "file.txt", range(6, 7)).unwrap();

        assert_eq!(
            project_exact(&git_repo, &anchor, &old).unwrap(),
            Projection::Relocated {
                path: "file.txt".to_owned(),
                lines: range(5, 6),
            }
        );
    }

    proptest::proptest! {
        /// Projection stability under content perturbation
        /// (`anchor.projection`): inserting lines strictly above the
        /// anchored range shifts it by exactly the insertion count, and
        /// appending lines strictly below leaves it untouched — for any
        /// file size, range, and insertion size.
        #[test]
        fn map_range_shifts_past_outside_edits_and_only_outside_edits(
            file_len in 1u64..200,
            range_start in 1u64..200,
            range_len in 0u64..20,
            inserted in 1u64..50,
        ) {
            proptest::prop_assume!(range_start + range_len <= file_len);
            let range = LineRange { start: range_start, end: range_start + range_len };
            let old: String = (1..=file_len).map(|n| format!("line {n}\n")).collect();

            // Insert `inserted` distinct lines at the very top. Even for a
            // range starting at line 1 this touches no anchored line — the
            // insertion hunk ends where the range begins — so it must
            // shift, never outdate.
            let above: String = (0..inserted)
                .map(|n| format!("inserted {n}\n"))
                .chain((1..=file_len).map(|n| format!("line {n}\n")))
                .collect();
            proptest::prop_assert_eq!(
                map_range(old.as_bytes(), above.as_bytes(), range),
                Some(LineRange { start: range.start + inserted, end: range.end + inserted })
            );

            // Append strictly below the range: the range must not move.
            let below: String = (1..=file_len)
                .map(|n| format!("line {n}\n"))
                .chain((0..inserted).map(|n| format!("appended {n}\n")))
                .collect();
            proptest::prop_assert_eq!(
                map_range(old.as_bytes(), below.as_bytes(), range),
                Some(range)
            );
        }
    }

    #[test]
    fn map_range_handles_edges() {
        let old = b"a\nb\nc\nd\n".as_slice();
        // An insertion exactly at the range start shifts it; one exactly
        // at its end leaves it alone.
        let above = b"x\na\nb\nc\nd\n".as_slice();
        assert_eq!(
            map_range(old, above, LineRange { start: 2, end: 3 }),
            Some(LineRange { start: 3, end: 4 })
        );
        // An insertion strictly inside the range outdates it.
        let inside = b"a\nb\nx\nc\nd\n".as_slice();
        assert_eq!(map_range(old, inside, LineRange { start: 2, end: 3 }), None);
        // A range past the end of the old file cannot map.
        assert_eq!(map_range(old, old, LineRange { start: 4, end: 9 }), None);
    }

    /// A copy of `anchor` whose recorded commit is a made-up id that was
    /// never written to the repository — standing in for "gc'd away"
    /// without actually having to run gc in a unit test; `has_object`
    /// answers `false` either way.
    fn with_missing_commit(anchor: &Anchor) -> Anchor {
        let mut forged = anchor.clone();
        let fake = gix::ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567").unwrap();
        forged.identity.genesis = fake.into();
        forged
    }

    #[test]
    fn project_exact_reports_the_anchor_commit_as_missing_and_project_degrades() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(&git_repo, "HEAD", "file.txt", range(5, 6)).unwrap();

        let edited = format!("added a\nadded b\n{}", numbered(1..=10));
        std::fs::write(dir.path().join("file.txt"), edited).unwrap();
        commit_all(dir.path(), "two");
        let git_repo = gix::open(dir.path()).unwrap();

        let anchor = with_missing_commit(&anchor);
        assert!(matches!(
            project_exact(&git_repo, &anchor, "HEAD"),
            Err(Error::AnchorCommitMissing(_))
        ));
        // The umbrella entry point degrades to the context fallback
        // instead of failing (`anchor.fuzzy-fallback`), and recovers the
        // same relocation the exact path would have found.
        assert_eq!(
            project(&git_repo, &anchor, "HEAD").unwrap(),
            Projection::Relocated {
                path: "file.txt".to_owned(),
                lines: range(7, 8),
            }
        );
    }

    /// Regression pinning the divergence `gix_anchor::project`'s doc comment
    /// now warns callers about: for a genuine rename, [`project_exact`] and
    /// [`project_from_context`] disagree once the anchor commit is gone,
    /// because [`project_from_context`] does no rename tracking at all (its
    /// own doc comment says so) and can only report the anchored path
    /// deleted. This is documented behavior, not an incidental bug --
    /// exactly why `project` (which silently picks between the two based on
    /// ambient GC state) is unsuitable for a cached or gating caller.
    #[test]
    fn project_exact_and_project_from_context_diverge_on_a_rename_once_gcd() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(&git_repo, "HEAD", "file.txt", range(3, 4)).unwrap();

        std::fs::rename(dir.path().join("file.txt"), dir.path().join("moved.txt")).unwrap();
        commit_all(dir.path(), "two");
        let git_repo = gix::open(dir.path()).unwrap();

        // While the anchor commit is still present, project_exact tracks
        // the rename via the tree diff.
        assert_eq!(
            project_exact(&git_repo, &anchor, "HEAD").unwrap(),
            Projection::Relocated {
                path: "moved.txt".to_owned(),
                lines: range(3, 4),
            }
        );

        // Once it's gone -- simulated gc, same as elsewhere in this suite --
        // project_exact fails outright, and project_from_context, with no
        // commit tree to diff against, reports the anchored path Deleted
        // for the exact same rename.
        let gcd = with_missing_commit(&anchor);
        assert!(matches!(
            project_exact(&git_repo, &gcd, "HEAD"),
            Err(Error::AnchorCommitMissing(_))
        ));
        assert_eq!(
            project_from_context(&git_repo, &gcd, "HEAD").unwrap(),
            Projection::Deleted
        );
    }

    /// [`project_many`] must agree with calling [`project_exact`] once per
    /// anchor -- the only thing it changes is resolving `target`'s tree
    /// once for the whole batch instead of once per anchor.
    #[test]
    fn project_many_agrees_with_project_exact_per_anchor() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        std::fs::write(dir.path().join("other.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor_edited_above = capture(&git_repo, "HEAD", "file.txt", range(3, 4)).unwrap();
        let anchor_deleted = capture(&git_repo, "HEAD", "other.txt", range(5, 6)).unwrap();
        let anchor_whole_file = capture(&git_repo, "HEAD", "file.txt", None).unwrap();

        std::fs::write(
            dir.path().join("file.txt"),
            format!("added a\nadded b\n{}", numbered(1..=10)),
        )
        .unwrap();
        std::fs::remove_file(dir.path().join("other.txt")).unwrap();
        std::fs::write(dir.path().join("unrelated.txt"), "different\n").unwrap();
        commit_all(dir.path(), "two");
        let git_repo = gix::open(dir.path()).unwrap();

        let anchors = vec![anchor_edited_above, anchor_deleted, anchor_whole_file];
        let batched = project_many(&git_repo, &anchors, "HEAD").unwrap();
        assert_eq!(batched.len(), anchors.len());
        for (anchor, result) in anchors.iter().zip(batched) {
            assert_eq!(
                result.unwrap(),
                project_exact(&git_repo, anchor, "HEAD").unwrap()
            );
        }
    }

    #[test]
    fn project_from_context_relocates_across_an_edit_above_the_range() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(&git_repo, "HEAD", "file.txt", range(5, 6)).unwrap();

        let edited = format!("added a\nadded b\n{}", numbered(1..=10));
        std::fs::write(dir.path().join("file.txt"), edited).unwrap();
        commit_all(dir.path(), "two");
        let git_repo = gix::open(dir.path()).unwrap();

        // Same answer `project_exact` would give, but derived with no
        // reference at all to the anchor's own (still very much present)
        // commit — exercising the exact code path that stands in once it
        // is gone.
        assert_eq!(
            project_from_context(&git_repo, &anchor, "HEAD").unwrap(),
            Projection::Relocated {
                path: "file.txt".to_owned(),
                lines: range(7, 8),
            }
        );
    }

    #[test]
    fn project_from_context_reports_outdated_when_no_window_matches_well() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(&git_repo, "HEAD", "file.txt", range(5, 6)).unwrap();

        // A wholesale rewrite leaves nothing resembling the captured
        // neighborhood anywhere in the file.
        std::fs::write(dir.path().join("file.txt"), "totally\nunrelated\ncontent\n").unwrap();
        commit_all(dir.path(), "two");
        let git_repo = gix::open(dir.path()).unwrap();

        assert_eq!(
            project_from_context(&git_repo, &anchor, "HEAD").unwrap(),
            Projection::Outdated {
                path: "file.txt".to_owned(),
            }
        );
    }

    #[test]
    fn project_from_context_reports_deleted() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(&git_repo, "HEAD", "file.txt", range(5, 6)).unwrap();

        std::fs::remove_file(dir.path().join("file.txt")).unwrap();
        std::fs::write(dir.path().join("unrelated.txt"), "different\n").unwrap();
        commit_all(dir.path(), "two");
        let git_repo = gix::open(dir.path()).unwrap();

        assert_eq!(
            project_from_context(&git_repo, &anchor, "HEAD").unwrap(),
            Projection::Deleted
        );
    }

    /// One *uncommitted* working-tree edit per taxonomy row of
    /// [`project_worktree_reports_the_spec_outcomes`] — the same rows the
    /// commit-target table enumerates, minus rename following, which the
    /// working tree deliberately degrades (`anchor.working-tree`).
    #[derive(Debug, Clone, Copy)]
    enum DirtyMutation {
        None,
        PrependTwoLines,
        EditLineFive,
        Delete,
        ReplaceWithDirectory,
    }

    impl DirtyMutation {
        fn apply(self, dir: &std::path::Path) {
            let file = dir.join("file.txt");
            match self {
                Self::None => {}
                Self::PrependTwoLines => {
                    std::fs::write(&file, format!("added a\nadded b\n{}", numbered(1..=10)))
                        .unwrap();
                }
                Self::EditLineFive => {
                    let edited = numbered(1..=10).replace("line 5\n", "line five\n");
                    std::fs::write(&file, edited).unwrap();
                }
                Self::Delete => {
                    std::fs::remove_file(&file).unwrap();
                }
                Self::ReplaceWithDirectory => {
                    std::fs::remove_file(&file).unwrap();
                    std::fs::create_dir(&file).unwrap();
                }
            }
        }
    }

    /// `anchor.working-tree`'s projection target: the four
    /// `anchor.projection` outcomes recovered against a dirty working
    /// tree, with no commit on the target side.
    #[rstest]
    #[case::unchanged_is_current(DirtyMutation::None, range(3, 4), Projection::Current)]
    #[case::edit_above_shifts(
        DirtyMutation::PrependTwoLines,
        range(5, 6),
        Projection::Relocated { path: "file.txt".to_owned(), lines: range(7, 8) }
    )]
    #[case::edit_inside_outdates(
        DirtyMutation::EditLineFive,
        range(5, 6),
        Projection::Outdated { path: "file.txt".to_owned() }
    )]
    #[case::deletion_is_deleted(DirtyMutation::Delete, range(3, 4), Projection::Deleted)]
    #[case::not_a_regular_file_outdates(
        DirtyMutation::ReplaceWithDirectory,
        range(3, 4),
        Projection::Outdated { path: "file.txt".to_owned() }
    )]
    #[case::whole_file_survives_an_edit(
        DirtyMutation::EditLineFive,
        None,
        Projection::Relocated { path: "file.txt".to_owned(), lines: None }
    )]
    fn project_worktree_reports_the_spec_outcomes(
        #[case] mutation: DirtyMutation,
        #[case] lines: Option<LineRange>,
        #[case] expected: Projection,
    ) {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(&git_repo, "HEAD", "file.txt", lines).unwrap();

        // Dirty the working tree only: nothing is committed, so only the
        // on-disk bytes can produce these outcomes.
        mutation.apply(dir.path());
        assert_eq!(
            project_worktree(&git_repo, &anchor, None).unwrap(),
            expected
        );
    }

    /// A caller-supplied buffer stands in for the on-disk bytes
    /// (`anchor.working-tree`): the projection follows the buffer, not the
    /// file — even when the file is gone entirely.
    #[test]
    fn project_worktree_prefers_a_caller_supplied_buffer_over_the_disk() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(&git_repo, "HEAD", "file.txt", range(5, 6)).unwrap();

        std::fs::remove_file(dir.path().join("file.txt")).unwrap();
        let buffer = format!("added a\nadded b\n{}", numbered(1..=10));
        assert_eq!(
            project_worktree(&git_repo, &anchor, Some(buffer.as_bytes())).unwrap(),
            Projection::Relocated {
                path: "file.txt".to_owned(),
                lines: range(7, 8),
            }
        );
        // Without the buffer, the same call reads the (deleted) disk state.
        assert_eq!(
            project_worktree(&git_repo, &anchor, None).unwrap(),
            Projection::Deleted
        );
    }

    /// A working-tree projection also works for an anchor that was itself
    /// captured from the working tree and whose bytes were never committed
    /// anywhere: the embedded content is the diff's old side, no commit
    /// participates (`anchor.working-tree`).
    #[test]
    fn project_worktree_needs_no_commit_on_either_side() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let dirty = numbered(1..=10).replace("line 9\n", "line nine\n");
        std::fs::write(dir.path().join("file.txt"), &dirty).unwrap();
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = crate::capture_worktree(&git_repo, "file.txt", range(5, 6)).unwrap();

        assert_eq!(
            project_worktree(&git_repo, &anchor, None).unwrap(),
            Projection::Current
        );
        std::fs::write(dir.path().join("file.txt"), format!("added a\n{dirty}")).unwrap();
        assert_eq!(
            project_worktree(&git_repo, &anchor, None).unwrap(),
            Projection::Relocated {
                path: "file.txt".to_owned(),
                lines: range(6, 7),
            }
        );
    }

    #[test]
    fn project_from_context_of_a_whole_file_anchor_survives_any_edit() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(&git_repo, "HEAD", "file.txt", None).unwrap();

        let edited = numbered(1..=10).replace("line 5\n", "line five\n");
        std::fs::write(dir.path().join("file.txt"), edited).unwrap();
        commit_all(dir.path(), "two");
        let git_repo = gix::open(dir.path()).unwrap();

        assert_eq!(
            project_from_context(&git_repo, &anchor, "HEAD").unwrap(),
            Projection::Relocated {
                path: "file.txt".to_owned(),
                lines: None,
            }
        );
    }
}
