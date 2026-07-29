//! Turning projected comments and threads into the LSP values the lens
//! publishes: code lenses (`lens.lenses`), hint diagnostics
//! (`lens.diagnostics`), and hover markup (`lens.hover`).
//!
//! Pure rendering only — every input is already-derived data (a [`Comment`],
//! a [`Thread`]), so the mapping from a comment to its on-screen shape is
//! unit-testable without a repository, and [`crate::lens::Lens`] owns the
//! per-request derivation that feeds it.

use gix::ObjectId;
use gix_anchor::{Anchor, LineRange, Projection};
use gix_comment::{Comment, State, Thread};
use lsp_types::{
    CodeLens, Command, Diagnostic, DiagnosticSeverity, MarkupContent, MarkupKind, Position, Range,
};
use serde_json::json;

/// The `workspace/executeCommand` command that opens/renders the thread
/// (`lens.lenses`: the view operation).
pub const CMD_VIEW: &str = "comment.view";
/// The command that starts a reply compose (`lens.lenses`, `lens.compose`).
pub const CMD_REPLY: &str = "comment.reply";
/// The command that resolves a comment (`lens.lenses`).
pub const CMD_RESOLVE: &str = "comment.resolve";
/// The command that reopens a resolved comment (`lens.lenses`).
pub const CMD_REOPEN: &str = "comment.reopen";

/// The diagnostic/lens source label the lens stamps every item with, so a
/// client can suppress just the conversation (`lens.diagnostics`).
pub const SOURCE: &str = "git-comment";

/// Where a projected comment lands on the open document, and whether its
/// anchored lines were edited out from under it (`Projection::Outdated`) —
/// `None` when the comment does not project onto the document at all
/// (`Projection::Deleted`), so the caller omits it (`lens.lenses`).
#[must_use]
pub fn landed_range(projection: &Projection, anchor: &Anchor) -> Option<(Range, bool)> {
    match projection {
        Projection::Current => Some((line_range(anchor.lines), false)),
        Projection::Relocated { lines, .. } => Some((line_range(*lines), false)),
        Projection::Outdated { .. } => Some((line_range(anchor.lines), true)),
        Projection::Deleted => None,
    }
}

/// The half-open LSP [`Range`] covering a 1-based inclusive line range, or
/// the document's first line for a whole-file anchor (`lines` is `None`).
fn line_range(lines: Option<LineRange>) -> Range {
    let (start, end) = match lines {
        Some(range) => (
            to_u32(range.start.saturating_sub(1)),
            to_u32(range.end.saturating_sub(1)),
        ),
        None => (0, 0),
    };
    Range {
        start: Position {
            line: start,
            character: 0,
        },
        // Extend to the end of the last line so a diagnostic underlines the
        // whole anchored region; clients clamp the character to line length.
        end: Position {
            line: end,
            character: u32::MAX,
        },
    }
}

fn to_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// A one-line summary of a comment message for a lens title or diagnostic
/// message: the first non-empty line, trimmed and capped so it fits inline.
#[must_use]
pub fn summary(message: &str) -> String {
    const CAP: usize = 60;
    let first = message
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    let mut chars = first.chars();
    let capped: String = chars.by_ref().take(CAP).collect();
    if chars.next().is_some() {
        format!("{capped}…")
    } else {
        capped
    }
}

/// The code lenses for one open root comment at `range` (`lens.lenses`): a
/// primary lens identifying the comment, summarizing its message, and
/// counting its replies, then Reply and Resolve lenses — the thread's
/// operations offered as commands that call the same library operations a
/// CLI porcelain would (`lens.parity`).
#[must_use]
pub fn code_lenses(
    id: ObjectId,
    comment: &Comment,
    range: Range,
    outdated: bool,
    replies: usize,
) -> Vec<CodeLens> {
    let mut title = format!("💬 {}: {}", short(id), summary(&comment.message));
    if replies == 1 {
        title.push_str(" (1 reply)");
    } else if replies > 1 {
        title.push_str(&format!(" ({replies} replies)"));
    }
    if outdated {
        title.push_str(" (outdated)");
    }
    let arg = vec![json!(id.to_string())];
    vec![
        CodeLens {
            range,
            command: Some(Command {
                title,
                command: CMD_VIEW.to_owned(),
                arguments: Some(arg.clone()),
            }),
            data: None,
        },
        CodeLens {
            range,
            command: Some(Command {
                title: "Reply".to_owned(),
                command: CMD_REPLY.to_owned(),
                arguments: Some(arg.clone()),
            }),
            data: None,
        },
        CodeLens {
            range,
            command: Some(Command {
                title: "Resolve".to_owned(),
                command: CMD_RESOLVE.to_owned(),
                arguments: Some(arg),
            }),
            data: None,
        },
    ]
}

/// The hint-severity diagnostic mirroring one open comment at `range`
/// (`lens.diagnostics`): the same conversation the code lens carries, for
/// clients that do not render lenses. Never a warning or an error.
#[must_use]
pub fn diagnostic(id: ObjectId, comment: &Comment, range: Range, outdated: bool) -> Diagnostic {
    let mut message = format!("{}: {}", short(id), summary(&comment.message));
    if outdated {
        message.push_str(" (outdated — the anchored lines changed)");
    }
    Diagnostic {
        range,
        // `lens.diagnostics` is binding: conversation carries no judgment,
        // so this is always a hint, never a warning or error.
        severity: Some(DiagnosticSeverity::HINT),
        code: None,
        code_description: None,
        source: Some(SOURCE.to_owned()),
        message,
        related_information: None,
        tags: None,
        data: None,
    }
}

/// The hover markup for a thread (`lens.hover`): every comment in it —
/// messages, states, and authorship — rendered as Markdown so the whole
/// conversation is readable in the buffer. Root first, then replies oldest
/// first (the order [`gix_comment::Comments::thread`] already returns).
#[must_use]
pub fn hover_markup(thread: &Thread) -> MarkupContent {
    let mut value = String::new();
    render_entry(&mut value, &thread.root, false);
    for reply in &thread.replies {
        value.push_str("\n---\n\n");
        render_entry(&mut value, reply, true);
    }
    MarkupContent {
        kind: MarkupKind::Markdown,
        value,
    }
}

fn render_entry(out: &mut String, comment: &Comment, is_reply: bool) {
    let reply_marker = if is_reply { "↳ " } else { "" };
    let when = comment
        .author
        .time
        .format(gix::date::time::format::ISO8601_STRICT)
        .unwrap_or_default();
    out.push_str(&format!(
        "**{reply_marker}{}** · `{}` · _{}_ · {when}\n\n",
        comment.author.name,
        state_str(comment.state),
        short(comment.id)
    ));
    out.push_str(comment.message.trim());
    out.push('\n');
}

fn state_str(state: State) -> &'static str {
    match state {
        State::Open => "open",
        State::Resolved => "resolved",
    }
}

/// A comment id shortened for display — the first seven characters, the
/// same length git uses for a short object id.
fn short(id: ObjectId) -> String {
    let hex = id.to_string();
    hex.get(..7).unwrap_or(&hex).to_owned()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "unit test")]

    use gix_comment::Author;

    use super::*;

    fn comment(message: &str, state: State) -> Comment {
        Comment {
            id: ObjectId::null(gix::hash::Kind::Sha1),
            target: ObjectId::null(gix::hash::Kind::Sha1),
            binding: gix_anchor::Binding::Commit {
                commit: ObjectId::null(gix::hash::Kind::Sha1).into(),
            },
            message: message.to_owned(),
            attachment: None,
            author: Author {
                name: "Ada".to_owned(),
                email: "ada@example.com".to_owned(),
                time: gix::date::Time::default(),
            },
            parent: None,
            state,
            commit: ObjectId::null(gix::hash::Kind::Sha1),
            created_at: 0,
        }
    }

    #[test]
    fn a_diagnostic_is_always_a_hint() {
        let diag = diagnostic(
            ObjectId::null(gix::hash::Kind::Sha1),
            &comment("hi", State::Open),
            line_range(None),
            false,
        );
        assert_eq!(diag.severity, Some(DiagnosticSeverity::HINT));
        assert_eq!(diag.source.as_deref(), Some(SOURCE));
    }

    #[test]
    fn lenses_offer_view_reply_resolve() {
        let lenses = code_lenses(
            ObjectId::null(gix::hash::Kind::Sha1),
            &comment("body text", State::Open),
            line_range(None),
            false,
            0,
        );
        let commands: Vec<&str> = lenses
            .iter()
            .filter_map(|lens| lens.command.as_ref().map(|c| c.command.as_str()))
            .collect();
        assert_eq!(commands, vec![CMD_VIEW, CMD_REPLY, CMD_RESOLVE]);
        let primary = lenses.first().unwrap().command.as_ref().unwrap();
        assert!(primary.title.contains("body text"));
    }

    #[test]
    fn a_reply_count_appears_in_the_primary_lens_title() {
        let lenses = code_lenses(
            ObjectId::null(gix::hash::Kind::Sha1),
            &comment("body", State::Open),
            line_range(None),
            false,
            2,
        );
        let primary = lenses.first().unwrap().command.as_ref().unwrap();
        assert!(primary.title.contains("2 replies"));
    }

    #[test]
    fn summary_caps_and_takes_the_first_nonempty_line() {
        assert_eq!(summary("\n\nfirst real line\nsecond"), "first real line");
        let long = "x".repeat(80);
        assert!(summary(&long).ends_with('…'));
    }

    #[test]
    fn hover_markup_includes_root_and_reply_messages() {
        let thread = Thread {
            root: comment("root message", State::Open),
            replies: vec![comment("reply message", State::Open)],
        };
        let markup = hover_markup(&thread);
        assert!(markup.value.contains("root message"));
        assert!(markup.value.contains("reply message"));
        assert!(markup.value.contains("↳"));
    }
}
