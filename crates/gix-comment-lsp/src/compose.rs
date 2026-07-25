//! The editor-file compose flow (`lens.compose`): building the template
//! git-style commit-message file the editor opens, and parsing it back
//! once the user saves.
//!
//! This module is pure — string in, string out, no IO and no git — so the
//! exact template grammar and the "an empty body aborts, `#` lines are
//! ignored" rule are unit-testable on their own, and [`crate::Lens`] owns
//! only the filesystem and mutation halves.
//!
//! # The mechanism, precisely
//!
//! `lens.compose` requires composing to work through a file "the way git
//! itself takes a commit message", using no client-specific extension. The
//! flow the lens drives, using only standard LSP a plain client provides
//! (`workspace/applyEdit`, `workspace/executeCommand`, and
//! `textDocument/didSave`):
//!
//! 1. A `textDocument/codeAction` on the selection returns a "Comment on
//!    these lines" action carrying a `WorkspaceEdit`; a lens's Reply
//!    command targets a parent instead and applies the same edit through
//!    `workspace/executeCommand` plus a server-sent `workspace/applyEdit`,
//!    since a code lens command has no `edit` field of its own.
//! 2. The edit's `CreateFile` operation creates a file under `.git/` named
//!    by [`template_filename`], and its `TextDocumentEdit` fills it in with
//!    [`template_text`]. Applying the edit is what gets the client to open
//!    the file — the same mechanism a "move to new file" refactor uses —
//!    rather than `window/showDocument`, which is not reliably supported
//!    for local files.
//! 3. The user edits the body and saves. The lens's `textDocument/didSave`
//!    handler reads the file, [`parse`]s it, and — if the body is
//!    non-empty — creates the comment through `gix_comment::Comments::add`
//!    or `::reply` (the same calls a CLI porcelain would make,
//!    `lens.parity`), anchoring to the working tree (`lens.working-tree`).
//!    An empty body aborts.
//!
//! The template is self-describing: the anchor target (path, lines, and an
//! optional reply parent) rides in `#`-prefixed metadata lines, so
//! [`parse`] recovers it from the saved file alone and the lens keeps no
//! per-compose state of its own. Because those lines start with `#` they
//! are ignored for the body exactly as any other comment line is, so the
//! metadata can never leak into the comment text. The filename itself also
//! names the target, for a human skimming `.git/`, and keeps two
//! concurrent composes (different lines, or a compose and a reply) from
//! colliding on one file.

/// The prefix every machine-readable metadata line in the template carries,
/// after the `#` comment marker: `# comment-lsp-<key>: <value>`.
const META_PREFIX: &str = "# comment-lsp-";

/// The template filename's fixed prefix — every compose/reply template
/// under `.git/` starts with this, so `Lens::did_save` recognizes one
/// without tracking any state of its own.
pub const TEMPLATE_PREFIX: &str = "COMMENT_EDITMSG";

/// What a compose targets: a path and optional `<start>[:<end>]` lines to
/// anchor against the working tree, and/or a reply parent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Target {
    /// Repository-relative path to anchor to, or `None` for a reply that
    /// inherits its aboutness from its parent.
    pub path: Option<String>,
    /// Lines to anchor, as `<start>[:<end>]`.
    pub lines: Option<String>,
    /// Id (hex) of the comment being replied to, when this compose is a
    /// reply.
    pub parent: Option<String>,
}

/// A parsed, saved template: the body (with `#` lines and surrounding
/// blank lines stripped) and the anchor [`Target`] its metadata named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Composed {
    /// The comment body the user typed. Empty (after trimming) means the
    /// compose was aborted (`lens.compose`).
    pub body: String,
    /// Where the comment anchors / who it replies to.
    pub target: Target,
}

impl Composed {
    /// Whether the saved template aborts the compose: an empty body once
    /// `#` lines and surrounding whitespace are stripped (`lens.compose`).
    #[must_use]
    pub fn is_abort(&self) -> bool {
        self.body.trim().is_empty()
    }
}

/// The template filename for `target`, under `.git/` — named after the
/// anchored path and lines (or the reply parent), so two composes in
/// flight at once never collide on one file.
#[must_use]
pub fn template_filename(target: &Target) -> String {
    if let Some(parent) = &target.parent {
        return format!("{TEMPLATE_PREFIX}_REPLY_{}", short(parent));
    }
    let path = target
        .path
        .as_deref()
        .unwrap_or("comment")
        .replace(['/', '\\'], "_");
    match &target.lines {
        Some(lines) => format!("{TEMPLATE_PREFIX}_{path}_{}", lines.replace(':', "-")),
        None => format!("{TEMPLATE_PREFIX}_{path}"),
    }
}

/// The initial template text for a new anchored comment on `path`/`lines`
/// against the working tree, or a reply when `target.parent` is set — a
/// blank body followed by git-style `#` guidance and the machine-readable
/// metadata [`parse`] reads back.
#[must_use]
pub fn template_text(target: &Target) -> String {
    let mut out = String::new();
    // One blank line for the body; the user types above the guidance.
    out.push('\n');
    out.push_str("# Leave a comment. Lines starting with '#' are ignored;\n");
    out.push_str("# an empty message aborts. Save this file to create the comment.\n");
    out.push_str("#\n");
    match (&target.parent, &target.path) {
        (Some(parent), _) => {
            out.push_str(&format!("# Replying to comment {parent}.\n"));
        }
        (None, Some(path)) => match &target.lines {
            Some(lines) => out.push_str(&format!("# On: {path} lines {lines} (working tree).\n")),
            None => out.push_str(&format!("# On: {path} (working tree).\n")),
        },
        (None, None) => {}
    }
    // Machine-readable metadata: one value per line, so a path containing
    // spaces round-trips without any escaping.
    if let Some(path) = &target.path {
        out.push_str(&format!("{META_PREFIX}path: {path}\n"));
    }
    if let Some(lines) = &target.lines {
        out.push_str(&format!("{META_PREFIX}lines: {lines}\n"));
    }
    if let Some(parent) = &target.parent {
        out.push_str(&format!("{META_PREFIX}parent: {parent}\n"));
    }
    out
}

/// Parse a saved template back into its body and [`Target`]
/// (`lens.compose`): every line starting with `#` is dropped from the body,
/// and the `# comment-lsp-<key>: <value>` metadata lines reconstruct the
/// anchor target the compose was started with.
#[must_use]
pub fn parse(content: &str) -> Composed {
    let mut target = Target::default();
    let mut body_lines: Vec<&str> = Vec::new();
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix(META_PREFIX) {
            if let Some((key, value)) = rest.split_once(':') {
                let value = value.trim().to_owned();
                match key {
                    "path" => target.path = Some(value),
                    "lines" => target.lines = Some(value),
                    "parent" => target.parent = Some(value),
                    _ => {}
                }
            }
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        body_lines.push(line);
    }
    let body = body_lines.join("\n").trim().to_owned();
    Composed { body, target }
}

/// A comment id shortened for use in a filename — the first twelve
/// characters, long enough to make an accidental collision between two
/// concurrently open reply templates practically impossible.
fn short(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_text_round_trips_through_parse() {
        let target = Target {
            path: Some("src/lib.rs".to_owned()),
            lines: Some("3:4".to_owned()),
            parent: None,
        };
        let text = template_text(&target);
        let composed = parse(&format!("hello there\n{text}"));
        assert_eq!(composed.body, "hello there");
        assert_eq!(composed.target, target);
    }

    #[test]
    fn an_empty_or_whitespace_body_aborts() {
        let target = Target::default();
        let text = template_text(&target);
        assert!(parse(&text).is_abort());
        assert!(parse(&format!("   \n\t\n{text}")).is_abort());
    }

    #[test]
    fn a_reply_template_round_trips_the_parent_and_no_path() {
        let target = Target {
            path: None,
            lines: None,
            parent: Some("abc123".to_owned()),
        };
        let text = template_text(&target);
        let composed = parse(&format!("a reply\n{text}"));
        assert_eq!(composed.body, "a reply");
        assert_eq!(composed.target.parent, Some("abc123".to_owned()));
        assert_eq!(composed.target.path, None);
    }

    #[test]
    fn filenames_differ_by_path_lines_and_reply_parent() {
        let a = template_filename(&Target {
            path: Some("src/lib.rs".to_owned()),
            lines: Some("1:2".to_owned()),
            parent: None,
        });
        let b = template_filename(&Target {
            path: Some("src/lib.rs".to_owned()),
            lines: Some("3:4".to_owned()),
            parent: None,
        });
        let c = template_filename(&Target {
            path: None,
            lines: None,
            parent: Some("deadbeef".to_owned()),
        });
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with(TEMPLATE_PREFIX));
        assert!(c.starts_with(TEMPLATE_PREFIX));
    }
}
