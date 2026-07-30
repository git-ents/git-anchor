//! The three named oracles (ARCHITECTURE.md, "git-anchor"): pure functions
//! of `(objects, Binding, params)` that map an [`Anchor`] onto a target
//! revision, each reporting `(oracle, confidence)` on every candidate it
//! finds. No oracle here applies a threshold anywhere — that is a rule's job
//! (`bind/5`, in `git-query`), not anchor's.
//!
//! [`op_log`] is a seam: no operation log exists yet anywhere in the product
//! family (DELTA X6), so it takes an [`OpLogSource`] the caller supplies and
//! is absent-safe — no source, no candidates, never an error. [`diff_trace`]
//! maps the anchored [`Span`] through hunks along a two-point tree diff
//! between the anchor's own commit and the target, with the diff algorithm
//! pinned as an oracle parameter ([`DIFF_TRACE_ALGORITHM`]) rather than read
//! from ambient configuration. [`fingerprint`] fuzzy-matches the anchor's
//! retained (or, if absent, freshly recomputed) content fingerprint against
//! candidate windows of the target blob.
//!
//! `project` — the pin-free chain of all three — is library-internal
//! (ARCHITECTURE.md, "`project` is library-internal"): no user-facing
//! command resolves through it; resolution is `git-query`'s `bind/5`. Each
//! named oracle function above, however, is public API, reachable for a
//! future `git-query` `bind` rewrite to build the confidence lattice from.

use gix::ObjectId;
use gix::bstr::ByteSlice as _;
use gix::diff::blob::{Algorithm, Diff, InternedInput};
use gix::diff::tree_with_rewrites::Change;

use crate::anchor::{Anchor, Span};
use crate::error::{Error, Result};
use crate::fingerprint::capture_fingerprint;
use crate::handle::AnchorId;
use crate::util::{commit_at, line_boundaries, read_blob, resolve_commit};

/// Which of the three oracles produced a [`Candidate`], highest fidelity
/// first (ARCHITECTURE.md's oracle table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Oracle {
    /// The external operation log ([`op_log`]) — highest fidelity.
    OpLog,
    /// Hunk-mapping along a first-parent-style two-point diff ([`diff_trace`]).
    DiffTrace,
    /// Fuzzy content matching ([`fingerprint`]).
    Fingerprint,
}

/// One place an anchor might sit on a target revision, as one [`Oracle`]
/// reports it, at that oracle's own reported confidence. Anchor applies no
/// threshold to `confidence` anywhere — every consumer (a rule, in
/// `git-query`) decides what to do with it.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Which oracle produced this candidate.
    pub oracle: Oracle,
    /// The oracle's own confidence in this candidate, in `0.0..=1.0`. Not a
    /// probability across oracles — each oracle defines its own scale.
    pub confidence: f64,
    /// The candidate path in the target tree.
    pub path: String,
    /// The candidate span in the target blob, or `None` when the candidate
    /// names a whole-file match with no narrower span to report.
    pub span: Option<Span>,
}

/// The op-log oracle's seam (ARCHITECTURE.md; DELTA X6): no op-log format,
/// store, or predicate exists yet anywhere in the product family, so this
/// crate defines only the trait a future `DeltaDB` adapter implements.
/// `git-anchor` depends on no concrete implementation.
pub trait OpLogSource {
    /// Every candidate the operation log records for `anchor_id` as of
    /// `target`.
    fn lookup(&self, anchor_id: AnchorId, target: ObjectId) -> Vec<Candidate>;
}

/// The `op-log` oracle (ARCHITECTURE.md's highest-fidelity mechanism):
/// delegates to `source` when the caller has one, and yields nothing —
/// never an error — when it does not, so an absent op-log degrades
/// silently to the other two oracles.
#[must_use]
pub fn op_log(
    source: Option<&dyn OpLogSource>,
    anchor_id: AnchorId,
    target: ObjectId,
) -> Vec<Candidate> {
    source.map_or_else(Vec::new, |source| source.lookup(anchor_id, target))
}

/// The diff algorithm [`diff_trace`] pins as an oracle parameter
/// (ARCHITECTURE.md: "diff algorithm + params pinned as oracle params"),
/// reproducible and citable rather than read from ambient configuration.
pub const DIFF_TRACE_ALGORITHM: Algorithm = Algorithm::Histogram;

/// Version of [`diff_trace`]'s rename-detection and hunk-mapping behavior.
/// Bump on any change that could make it answer differently for the same
/// inputs — a `gix` upgrade included — so a caller that caches a candidate
/// (or something derived from one) can fold this into its cache key.
pub const DIFF_TRACE_ORACLE_VERSION: u32 = 1;

/// The `diff-trace` oracle (ARCHITECTURE.md): diffs `anchor`'s own commit
/// tree against `target`'s (with rename tracking, using
/// [`DIFF_TRACE_ALGORITHM`]) and maps `anchor.identity.span` through the
/// resulting hunks.
///
/// Yields exactly one candidate at confidence `1.0` when the anchored region
/// survives intact anywhere in the target tree (renamed or not, at its
/// original path or not), and no candidates — never an error — when
/// `anchor`'s own commit is unreachable (retained on a best-effort basis
/// only), the anchored path was deleted, or the anchored region itself was
/// edited: exact history tracing has nothing to report in any of those
/// cases, and [`fingerprint`] is the fallback a caller composes in.
pub fn diff_trace(repo: &gix::Repository, anchor: &Anchor, target: &str) -> Result<Vec<Candidate>> {
    let target_commit = resolve_commit(repo, target)?;
    let target_tree = target_commit
        .tree()
        .map_err(|error| Error::Object(error.to_string()))?;
    diff_trace_onto(repo, anchor, &target_tree)
}

/// [`diff_trace`]'s implementation, taking an already-resolved
/// `target_tree` so a caller revisiting many anchors against the same
/// target can resolve it once.
fn diff_trace_onto(
    repo: &gix::Repository,
    anchor: &Anchor,
    target_tree: &gix::Tree<'_>,
) -> Result<Vec<Candidate>> {
    let anchor_commit_id = ObjectId::from(anchor.identity.genesis_rev);
    if !repo.has_object(anchor_commit_id) {
        return Ok(Vec::new());
    }
    let anchor_commit = commit_at(repo, anchor_commit_id)?;
    let anchor_tree = anchor_commit
        .tree()
        .map_err(|error| Error::Object(error.to_string()))?;
    let Some(anchor_blob) = blob_at(&anchor_tree, &anchor.identity.path)? else {
        return Ok(Vec::new());
    };

    if let Some(entry) = target_tree
        .lookup_entry_by_path(&anchor.identity.path)
        .map_err(|error| Error::Object(error.to_string()))?
        && entry.mode().is_blob()
        && entry.object_id() == anchor_blob
    {
        return Ok(vec![candidate(
            &anchor.identity.path,
            Some(anchor.identity.span),
        )]);
    }

    // Rename tracking is pinned to git's defaults (50% similarity, no
    // copies) rather than read from repository configuration, so a
    // candidate is the same answer everywhere the repository is checked out.
    let options = gix::diff::Options::default().with_rewrites(Some(gix::diff::Rewrites::default()));
    let changes = repo
        .diff_tree_to_tree(Some(&anchor_tree), Some(target_tree), options)
        .map_err(|error| Error::Diff(error.to_string()))?;

    let mut destination: Option<(String, ObjectId, bool)> = None;
    for change in changes {
        match change {
            Change::Deletion { location, .. }
                if location.as_bytes() == anchor.identity.path.as_bytes() =>
            {
                return Ok(Vec::new());
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
        return Ok(Vec::new());
    };
    if !is_blob {
        return Ok(Vec::new());
    }
    if blob == anchor_blob {
        return Ok(vec![candidate(&path, Some(anchor.identity.span))]);
    }
    let old_bytes = read_blob(repo, anchor_blob)?;
    let new_bytes = read_blob(repo, blob)?;
    match map_span(&old_bytes, &new_bytes, anchor.identity.span) {
        Some(mapped) => Ok(vec![candidate(&path, Some(mapped))]),
        None => Ok(Vec::new()),
    }
}

/// A confidence-`1.0` [`Oracle::DiffTrace`] candidate at `path`/`span`.
fn candidate(path: &str, span: Option<Span>) -> Candidate {
    Candidate {
        oracle: Oracle::DiffTrace,
        confidence: 1.0,
        path: path.to_owned(),
        span,
    }
}

/// The blob id at `path` in `tree`, or `None` when absent or not a blob.
fn blob_at(tree: &gix::Tree<'_>, path: &str) -> Result<Option<ObjectId>> {
    Ok(tree
        .lookup_entry_by_path(path)
        .map_err(|error| Error::Object(error.to_string()))?
        .filter(|entry| entry.mode().is_blob())
        .map(|entry| entry.object_id()))
}

/// Map `span` from `old`'s bytes to `new`'s by walking the line-hunk diff
/// between them: a hunk entirely above the span shifts it by the hunk's
/// growth, a hunk entirely below is ignored, and any hunk touching the span
/// means the anchored region itself changed, reported as `None` rather than
/// guessed at.
fn map_span(old: &[u8], new: &[u8], span: Span) -> Option<Span> {
    let (start_line, end_line) = crate::util::span_to_lines(old, span);
    if end_line < start_line {
        return None;
    }
    let input = InternedInput::new(old, new);
    if end_line > u64::try_from(input.before.len()).ok()? {
        return None;
    }
    let diff = Diff::compute(DIFF_TRACE_ALGORITHM, &input);
    let mut added: u64 = 0;
    let mut removed: u64 = 0;
    for hunk in diff.hunks() {
        let before_start = u64::from(hunk.before.start);
        let before_end = u64::from(hunk.before.end);
        if before_end <= start_line {
            removed = removed.checked_add(before_end.checked_sub(before_start)?)?;
            added = added
                .checked_add(u64::from(hunk.after.end).checked_sub(u64::from(hunk.after.start))?)?;
        } else if before_start >= end_line {
            break;
        } else {
            return None;
        }
    }
    let map = |line: u64| line.checked_add(added)?.checked_sub(removed);
    crate::util::lines_to_span(new, map(start_line)?, map(end_line)?)
}

/// The `fingerprint` oracle (ARCHITECTURE.md): fuzzy content search for
/// `anchor` in `target`'s version of `anchor.identity.path`, using
/// [`Anchor::hints`]'s retained fingerprint — recomputed from `anchor`'s own
/// commit when absent, per ARCHITECTURE.md ("recomputes if absent").
///
/// Slides a window the width of `anchor.identity.span` across every line
/// boundary of the target blob, scoring each by [`minhash_similarity`]
/// against the reference fingerprint, and reports every window that scores
/// above zero as its own candidate — ranking and thresholding are a rule's
/// job, not this oracle's.
pub fn fingerprint(
    repo: &gix::Repository,
    anchor: &Anchor,
    target: &str,
) -> Result<Vec<Candidate>> {
    let reference = match anchor.hints.fingerprints.first() {
        Some(fingerprint) => fingerprint.value.clone(),
        None => match recompute_reference(repo, anchor)? {
            Some(value) => value,
            None => return Ok(Vec::new()),
        },
    };

    let target_commit = resolve_commit(repo, target)?;
    let target_tree = target_commit
        .tree()
        .map_err(|error| Error::Object(error.to_string()))?;
    let Some(entry) = target_tree
        .lookup_entry_by_path(&anchor.identity.path)
        .map_err(|error| Error::Object(error.to_string()))?
        .filter(|entry| entry.mode().is_blob())
    else {
        return Ok(Vec::new());
    };
    let data = read_blob(repo, entry.object_id())?;

    let window_len = usize::try_from(
        anchor
            .identity
            .span
            .end
            .saturating_sub(anchor.identity.span.start),
    )
    .unwrap_or(0);
    if window_len == 0 || window_len > data.len() {
        return Ok(Vec::new());
    }

    let mut candidates = Vec::new();
    for &start in &line_boundaries(&data) {
        let Ok(start_usize) = usize::try_from(start) else {
            continue;
        };
        let Some(end_usize) = start_usize.checked_add(window_len) else {
            continue;
        };
        let Some(window) = data.get(start_usize..end_usize) else {
            continue;
        };
        let score = minhash_similarity(&capture_fingerprint(window).value, &reference);
        if score > 0.0 {
            candidates.push(Candidate {
                oracle: Oracle::Fingerprint,
                confidence: score,
                path: anchor.identity.path.clone(),
                span: Some(Span {
                    start,
                    end: u64::try_from(end_usize).unwrap_or(u64::MAX),
                }),
            });
        }
    }
    Ok(candidates)
}

/// Recompute `anchor`'s own fingerprint from its recorded commit, when
/// [`Anchor::hints`] carries none — `None` (never an error) when the commit
/// or path is no longer reachable, since there is then nothing to recompute
/// from.
fn recompute_reference(repo: &gix::Repository, anchor: &Anchor) -> Result<Option<Vec<u8>>> {
    let commit_id = ObjectId::from(anchor.identity.genesis_rev);
    if !repo.has_object(commit_id) {
        return Ok(None);
    }
    let commit = commit_at(repo, commit_id)?;
    let tree = commit
        .tree()
        .map_err(|error| Error::Object(error.to_string()))?;
    let Some(blob) = blob_at(&tree, &anchor.identity.path)? else {
        return Ok(None);
    };
    let bytes = read_blob(repo, blob)?;
    let Some(slice) = anchor.identity.span.slice(&bytes) else {
        return Ok(None);
    };
    Ok(Some(capture_fingerprint(slice).value))
}

/// The fraction of matching 8-byte MinHash slots between `a` and `b` — a
/// standard MinHash similarity estimate, `0.0` when the two fingerprints are
/// not the same shape (different algorithm or parameters) to compare at all.
#[must_use]
pub fn minhash_similarity(a: &[u8], b: &[u8]) -> f64 {
    if a.is_empty() || a.len() != b.len() || !a.len().is_multiple_of(8) {
        return 0.0;
    }
    let slots = a.len() / 8;
    let matches = (0..slots)
        .filter(|&i| a[i * 8..i * 8 + 8] == b[i * 8..i * 8 + 8])
        .count();
    #[allow(
        clippy::cast_precision_loss,
        reason = "slot counts are tiny (single digits)"
    )]
    let ratio = matches as f64 / slots as f64;
    ratio
}

/// The pin-free oracle chain (ARCHITECTURE.md: "`project` = the pin-free
/// oracle chain. Library-internal."): every candidate [`op_log`],
/// [`diff_trace`], and [`fingerprint`] report, concatenated in that
/// (highest-to-lowest fidelity) order. No user-facing command resolves
/// through this — `crates/git-anchor` calls none of it; resolution is
/// `git-query`'s `bind/5`, built from the three named functions above.
#[allow(
    dead_code,
    reason = "library-internal chain; exercised by tests today, by a future in-crate consumer later"
)]
pub(crate) fn project(
    repo: &gix::Repository,
    anchor: &Anchor,
    anchor_id: AnchorId,
    target: &str,
    op_log_source: Option<&dyn OpLogSource>,
) -> Result<Vec<Candidate>> {
    let target_commit = resolve_commit(repo, target)?;
    let target_id = target_commit.id().detach();
    let mut candidates = op_log(op_log_source, anchor_id, target_id);
    candidates.extend(diff_trace(repo, anchor, target)?);
    candidates.extend(fingerprint(repo, anchor, target)?);
    Ok(candidates)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "unit test")]

    use rstest::rstest;

    use super::*;
    use crate::anchor::{LineRange, capture};
    use crate::fixture::{commit_all, numbered, repo};

    fn range(start: u64, end: u64) -> Option<LineRange> {
        Some(LineRange { start, end })
    }

    #[test]
    fn op_log_yields_nothing_when_absent() {
        let anchor_id = AnchorId::from(gix::ObjectId::null(gix::hash::Kind::Sha1));
        let target = gix::ObjectId::null(gix::hash::Kind::Sha1);
        assert_eq!(op_log(None, anchor_id, target), Vec::new());
    }

    #[test]
    fn op_log_delegates_to_a_supplied_source() {
        struct OneCandidate;
        impl OpLogSource for OneCandidate {
            fn lookup(&self, _anchor_id: AnchorId, _target: ObjectId) -> Vec<Candidate> {
                vec![Candidate {
                    oracle: Oracle::OpLog,
                    confidence: 1.0,
                    path: "src/lib.rs".to_owned(),
                    span: None,
                }]
            }
        }
        let anchor_id = AnchorId::from(gix::ObjectId::null(gix::hash::Kind::Sha1));
        let target = gix::ObjectId::null(gix::hash::Kind::Sha1);
        let candidates = op_log(Some(&OneCandidate), anchor_id, target);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].oracle, Oracle::OpLog);
    }

    #[rstest]
    #[case::unchanged("other.txt")]
    fn diff_trace_reports_current_at_confidence_one(#[case] _unused: &str) {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(&git_repo, "HEAD", "file.txt", range(3, 4)).unwrap();

        let candidates = diff_trace(&git_repo, &anchor, "HEAD").unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].confidence, 1.0);
        assert_eq!(candidates[0].path, "file.txt");
        assert_eq!(candidates[0].span, Some(anchor.identity.span));
    }

    #[test]
    fn diff_trace_shifts_the_span_across_an_edit_above() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(&git_repo, "HEAD", "file.txt", range(5, 6)).unwrap();

        std::fs::write(
            dir.path().join("file.txt"),
            format!("added a\nadded b\n{}", numbered(1..=10)),
        )
        .unwrap();
        commit_all(dir.path(), "two");
        let git_repo = gix::open(dir.path()).unwrap();

        let candidates = diff_trace(&git_repo, &anchor, "HEAD").unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].confidence, 1.0);
        // The insertion moves the anchored text two lines down; its own
        // content (which happens to read "line 5\nline 6\n", same as its
        // original 1-based line numbers) is unchanged by the move.
        let expected = "line 5\nline 6\n";
        let full = format!("added a\nadded b\n{}", numbered(1..=10));
        assert_eq!(
            candidates[0].span.unwrap().slice(full.as_bytes()).unwrap(),
            expected.as_bytes()
        );
    }

    #[test]
    fn diff_trace_yields_nothing_when_the_region_was_edited() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(&git_repo, "HEAD", "file.txt", range(5, 6)).unwrap();

        let edited = numbered(1..=10).replace("line 5\n", "line five\n");
        std::fs::write(dir.path().join("file.txt"), edited).unwrap();
        commit_all(dir.path(), "two");
        let git_repo = gix::open(dir.path()).unwrap();

        assert_eq!(diff_trace(&git_repo, &anchor, "HEAD").unwrap(), Vec::new());
    }

    #[test]
    fn diff_trace_yields_nothing_when_the_anchor_commit_is_gone() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let mut anchor = capture(&git_repo, "HEAD", "file.txt", range(5, 6)).unwrap();
        anchor.identity.genesis_rev =
            gix::ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567")
                .unwrap()
                .into();

        assert_eq!(diff_trace(&git_repo, &anchor, "HEAD").unwrap(), Vec::new());
    }

    #[test]
    fn fingerprint_falls_back_to_fuzzy_matching_once_the_anchor_commit_is_gone() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let mut anchor = capture(&git_repo, "HEAD", "file.txt", range(5, 6)).unwrap();

        let edited = format!("added a\nadded b\n{}", numbered(1..=10));
        std::fs::write(dir.path().join("file.txt"), edited).unwrap();
        commit_all(dir.path(), "two");
        let git_repo = gix::open(dir.path()).unwrap();

        // Simulate the anchor's own commit having been gc'd: `diff_trace`
        // has nothing to work with, but the retained fingerprint survives.
        anchor.identity.genesis_rev =
            gix::ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567")
                .unwrap()
                .into();
        assert_eq!(diff_trace(&git_repo, &anchor, "HEAD").unwrap(), Vec::new());

        let candidates = fingerprint(&git_repo, &anchor, "HEAD").unwrap();
        assert!(!candidates.is_empty());
        let best = candidates
            .iter()
            .max_by(|a, b| a.confidence.total_cmp(&b.confidence))
            .unwrap();
        assert_eq!(best.confidence, 1.0);
        let full = format!("added a\nadded b\n{}", numbered(1..=10));
        assert_eq!(
            best.span.unwrap().slice(full.as_bytes()).unwrap(),
            "line 5\nline 6\n".as_bytes()
        );
    }

    #[test]
    fn minhash_similarity_is_one_for_equal_fingerprints_and_zero_for_mismatched_shapes() {
        let fp = capture_fingerprint(b"the quick brown fox");
        assert_eq!(minhash_similarity(&fp.value, &fp.value), 1.0);
        assert_eq!(minhash_similarity(&fp.value, &[0u8; 3]), 0.0);
    }

    #[test]
    fn project_chains_diff_trace_then_fingerprint() {
        let dir = repo();
        std::fs::write(dir.path().join("file.txt"), numbered(1..=10)).unwrap();
        commit_all(dir.path(), "one");
        let git_repo = gix::open(dir.path()).unwrap();
        let anchor = capture(&git_repo, "HEAD", "file.txt", range(3, 4)).unwrap();
        let anchor_id = anchor.id().unwrap();

        let candidates = project(&git_repo, &anchor, anchor_id, "HEAD", None).unwrap();
        assert!(candidates.iter().any(|c| c.oracle == Oracle::DiffTrace));
    }
}
