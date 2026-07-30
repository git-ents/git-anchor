//! Small `gix` plumbing helpers shared by [`crate::capture`] and
//! [`crate::oracle`].
//!
//! Nothing here is public API; each function is a thin, single-purpose
//! wrapper over a `gix::Repository` lookup or a line/byte conversion, kept
//! out of the call sites that use it so the oracle and capture logic reads
//! as policy rather than plumbing.

use gix::ObjectId;
use gix::bstr::ByteSlice as _;

use crate::anchor::{LineRange, Span};
use crate::error::{Error, Result};

/// Resolve `revision` (a hex id, ref name, or revspec) to the commit it
/// names in `repo`.
pub(crate) fn resolve_commit<'repo>(
    repo: &'repo gix::Repository,
    revision: &str,
) -> Result<gix::Commit<'repo>> {
    let resolve = || Error::Resolve(revision.to_owned());
    repo.rev_parse_single(revision)
        .map_err(|_error| resolve())?
        .object()
        .map_err(|_error| resolve())?
        .peel_to_kind(gix::object::Kind::Commit)
        .map_err(|_error| resolve())?
        .try_into_commit()
        .map_err(|_error| resolve())
}

/// Look up the commit `id` names directly, with no revision parsing — for an
/// [`crate::Anchor`]'s own recorded commit, which already names a concrete
/// object rather than an arbitrary revision.
pub(crate) fn commit_at(repo: &gix::Repository, id: ObjectId) -> Result<gix::Commit<'_>> {
    let resolve = || Error::Resolve(id.to_string());
    repo.find_object(id)
        .map_err(|_error| resolve())?
        .peel_to_kind(gix::object::Kind::Commit)
        .map_err(|_error| resolve())?
        .try_into_commit()
        .map_err(|_error| resolve())
}

/// Read the full contents of the blob at `id`.
pub(crate) fn read_blob(repo: &gix::Repository, id: ObjectId) -> Result<Vec<u8>> {
    Ok(repo
        .find_blob(id)
        .map_err(|error| Error::Object(error.to_string()))?
        .take_data())
}

/// The byte offsets of every line's start in `data`, plus a final entry at
/// `data.len()` — `n` lines produce `n + 1` boundaries, so line `i` (0-based)
/// spans `boundaries[i]..boundaries[i + 1]`.
pub(crate) fn line_boundaries(data: &[u8]) -> Vec<u64> {
    let mut boundaries = vec![0u64];
    let mut offset = 0u64;
    for line in data.lines_with_terminator() {
        offset = offset.saturating_add(u64::try_from(line.len()).unwrap_or(u64::MAX));
        boundaries.push(offset);
    }
    boundaries
}

/// The canonical [`Span`] of the 1-based inclusive `range` within `data`
/// (`anchor.definition`'s validation, applied at capture time — the byte
/// span it produces is what [`crate::anchor::AnchorIdentity`] actually
/// carries), or [`Error::LinesOutOfRange`] (naming `path`) when the range
/// does not fit.
pub(crate) fn byte_span_of(data: &[u8], path: &str, range: LineRange) -> Result<Span> {
    let boundaries = line_boundaries(data);
    let line_count = boundaries.len().saturating_sub(1);
    let out_of_range = || Error::LinesOutOfRange {
        path: path.to_owned(),
        start: range.start,
        end: range.end,
        len: u64::try_from(line_count).unwrap_or(u64::MAX),
    };
    let first = usize::try_from(range.start)
        .ok()
        .and_then(|start| start.checked_sub(1))
        .ok_or_else(out_of_range)?;
    let last = usize::try_from(range.end).ok().ok_or_else(out_of_range)?;
    if first > last || last > line_count {
        return Err(out_of_range());
    }
    Ok(Span {
        start: boundaries[first],
        end: boundaries[last],
    })
}

/// The 0-based half-open line range `span` covers in `data`: every line any
/// byte of `span` touches. An empty span at a line boundary covers no lines.
pub(crate) fn span_to_lines(data: &[u8], span: Span) -> (u64, u64) {
    let boundaries = line_boundaries(data);
    let start_line = boundaries
        .iter()
        .rposition(|&b| b <= span.start)
        .unwrap_or(0);
    let end_line = if span.end <= span.start {
        start_line
    } else {
        boundaries
            .iter()
            .position(|&b| b >= span.end)
            .unwrap_or(boundaries.len().saturating_sub(1))
    };
    (
        u64::try_from(start_line).unwrap_or(0),
        u64::try_from(end_line).unwrap_or(0),
    )
}

/// The [`Span`] covering 0-based half-open line range `start_line..end_line`
/// in `data` — the inverse of [`span_to_lines`], used once hunk mapping has
/// relocated a line range to map it back to bytes on the new side.
pub(crate) fn lines_to_span(data: &[u8], start_line: u64, end_line: u64) -> Option<Span> {
    let boundaries = line_boundaries(data);
    let start = usize::try_from(start_line).ok()?;
    let end = usize::try_from(end_line).ok()?;
    Some(Span {
        start: *boundaries.get(start)?,
        end: *boundaries.get(end)?,
    })
}
