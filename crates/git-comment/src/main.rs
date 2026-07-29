//! `git-comment`: a git external subcommand (`git comment …`) that pins a
//! message to a Git object — a commit itself, or a durable position within
//! one (a line range in a blob at a commit) — the
//! [`gix_comment::Binding`] vocabulary, driven from the shell.
//!
//! A comment's author and timestamp are never given on the command line:
//! they are the storage commit's, recorded by git itself the moment the
//! comment (or a new version of it) is committed. `add` attaches a comment:
//! to a bare revision (`Binding::Commit`) or, with `--path`, to a specific
//! blob path and optional line range (`Binding::Position`, a
//! [`gix_comment::Anchor`]) — either way, optionally carrying a raw-tree
//! attachment (`--attach`) alongside the message. `reply` starts a new
//! comment that joins an existing one's thread instead of standing alone;
//! `resolve`/`reopen` flip a comment's lifecycle state without touching its
//! message. `list` and `show` read comments back — `show <id>@<rev>`
//! projects a position-bound comment onto another commit, re-deriving where
//! its anchor now sits, the way git addresses a revision; `show <id>~N`
//! reads an older version of the comment itself; `show <id> --thread` prints
//! the comment's whole thread, root first. `edit` and `append` reattach a
//! new or extended message; `log` prints a comment's version history.
//! `remove` deletes one or more comments. Bare `git comment` lists, like
//! `git remote`.

use std::collections::HashMap;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use gix::ObjectId;
use gix_comment::{
    Anchor, Binding, Comment, Comments, LineRange, Projection, State, Thread, capture,
    capture_worktree, project, project_worktree, snippet,
};

#[derive(Parser)]
#[command(name = "git-comment", about = "Pin a message to a Git object", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Attach a comment to a revision (defaults to `HEAD`), or, with
    /// `--path`, to a specific blob path and optional line range within it.
    Add(AddArgs),
    /// Reply to an existing comment, joining its thread instead of starting
    /// a new one. The reply inherits the parent's binding automatically.
    Reply(ReplyArgs),
    /// Replace a comment's message. With no `-m`/`-F` and nothing piped,
    /// opens `$EDITOR` seeded with the current message.
    Edit(EditArgs),
    /// Append to a comment's message, separated by a blank line — the new
    /// content is gathered the same way `add`'s message is.
    Append(AppendArgs),
    /// Mark a comment resolved, message and attachment unchanged.
    Resolve {
        /// A comment id (an unambiguous hex prefix is fine).
        id: String,
    },
    /// Mark a resolved comment open again.
    Reopen {
        /// A comment id (an unambiguous hex prefix is fine).
        id: String,
    },
    /// List comment threads, or only those attached to `<object>`. Lists
    /// open thread roots by default; `--resolved`/`--all` widen that.
    #[command(visible_alias = "ls")]
    List(ListArgs),
    /// Show a comment's target, binding, author, message, and state. Append
    /// `@<rev>` to a position-bound comment's id to project it onto another
    /// commit instead, re-deriving where its anchor now sits; append `~N`
    /// or `^` to see an older version of the comment itself; `--thread`
    /// shows the whole thread instead of just this comment.
    Show(ShowArgs),
    /// Show a comment's version history, newest first.
    Log {
        /// A comment id (an unambiguous hex prefix is fine).
        id: String,
    },
    /// Remove one or more comments.
    #[command(visible_alias = "rm")]
    Remove {
        /// One or more comment ids (unambiguous hex prefixes are fine).
        /// Every id is resolved before any comment is removed, so an
        /// ambiguous or missing id leaves all comments untouched.
        ids: Vec<String>,
    },
    /// Run an editor-facing Language Server Protocol view over
    /// `refs/comments/*` on stdio (code lenses, hover threads, hint
    /// diagnostics, and a compose-on-save flow for new comments). For
    /// editor integration, not interactive use.
    Lsp,
}

/// Arguments for `add`.
#[derive(clap::Args)]
struct AddArgs {
    /// The revision to attach to. Defaults to `HEAD`. Conflicts with
    /// `--worktree`, which anchors the working tree instead of a revision.
    #[arg(conflicts_with = "worktree")]
    object: Option<String>,
    /// Anchor a specific blob path (as it exists at `<object>`) instead of
    /// the revision itself. Resolved relative to the current directory, the
    /// same way git pathspecs are.
    #[arg(long = "path", value_name = "PATH")]
    path: Option<String>,
    /// Anchor a line range within the path: `start,end` (1-based,
    /// inclusive), `start,+count`, or a single line number alone. A
    /// trailing `:path` supplies the path directly (as `git log -L` accepts)
    /// — an error if `--path` is also given and disagrees. Requires a path
    /// from one source or the other (checked immediately, before any git
    /// work, since clap can't express "requires `--path`, unless this
    /// value's own `:path` supplies one").
    #[arg(
        short = 'L',
        long = "lines",
        value_name = "START,END[:PATH]",
        value_parser = parse_lines_arg
    )]
    lines: Option<LinesArg>,
    /// The comment message, taken verbatim.
    #[arg(
        short = 'm',
        long = "message",
        value_name = "MSG",
        conflicts_with = "file"
    )]
    message: Option<String>,
    /// Read the comment message from a file.
    #[arg(short = 'F', long = "file", value_name = "FILE")]
    file: Option<PathBuf>,
    /// Anchor the working tree's on-disk content at `--path` instead of a
    /// committed revision. Requires `--path`; conflicts with `<object>`.
    #[arg(long, requires = "path", conflicts_with = "object")]
    worktree: bool,
    /// Attach an arbitrary tree-ish's tree alongside the message, embedded
    /// in the comment so it stays reachable through the comment's own ref.
    #[arg(long = "attach", value_name = "TREE-ISH")]
    attach: Option<String>,
}

/// Arguments for `reply`.
#[derive(clap::Args)]
struct ReplyArgs {
    /// The comment being replied to — an unambiguous hex prefix is fine.
    id: String,
    /// The reply's message, taken verbatim.
    #[arg(
        short = 'm',
        long = "message",
        value_name = "MSG",
        conflicts_with = "file"
    )]
    message: Option<String>,
    /// Read the reply's message from a file.
    #[arg(short = 'F', long = "file", value_name = "FILE")]
    file: Option<PathBuf>,
    /// Attach an arbitrary tree-ish's tree alongside the reply.
    #[arg(long = "attach", value_name = "TREE-ISH")]
    attach: Option<String>,
}

/// Arguments for `edit`.
#[derive(clap::Args)]
struct EditArgs {
    /// A comment id, or an unambiguous hex prefix of one.
    id: String,
    /// The new message, taken verbatim.
    #[arg(
        short = 'm',
        long = "message",
        value_name = "MSG",
        conflicts_with = "file"
    )]
    message: Option<String>,
    /// Read the message from a file.
    #[arg(short = 'F', long = "file", value_name = "FILE")]
    file: Option<PathBuf>,
    /// Replace the attachment with an arbitrary tree-ish's tree. Omit to
    /// keep the comment's existing attachment (if any) unchanged.
    #[arg(long = "attach", value_name = "TREE-ISH")]
    attach: Option<String>,
}

/// Arguments for `append`.
#[derive(clap::Args)]
struct AppendArgs {
    /// A comment id, or an unambiguous hex prefix of one.
    id: String,
    /// The additional message, taken verbatim.
    #[arg(
        short = 'm',
        long = "message",
        value_name = "MSG",
        conflicts_with = "file"
    )]
    message: Option<String>,
    /// Read the additional message from a file.
    #[arg(short = 'F', long = "file", value_name = "FILE")]
    file: Option<PathBuf>,
}

/// Arguments for `list`.
#[derive(clap::Args)]
struct ListArgs {
    /// A revision to filter comments down to those attached to it
    /// (including a position comment whose anchor was captured at that
    /// commit).
    object: Option<String>,
    /// Emit one JSON object per line instead of the human-readable columns.
    #[arg(long)]
    json: bool,
    /// Include resolved thread roots alongside open ones (open roots only by
    /// default). Conflicts with `--all`, which drops the roots-only filter
    /// entirely.
    #[arg(long, conflicts_with = "all")]
    resolved: bool,
    /// List every comment — thread roots and replies alike, any state —
    /// instead of just open roots.
    #[arg(long)]
    all: bool,
}

/// Arguments for `show`.
#[derive(clap::Args)]
struct ShowArgs {
    /// A comment id (an unambiguous hex prefix is fine), optionally with an
    /// `@<rev>` suffix to project onto that revision, or a `~N`/`^` suffix
    /// to read an older version of the comment.
    spec: String,
    /// Emit a machine-readable object instead of the human-readable form.
    #[arg(long)]
    json: bool,
    /// Project onto the working tree instead of showing the captured
    /// location. Conflicts with an `@<rev>` or `~N`/`^` suffix on `spec`.
    #[arg(long, conflicts_with = "thread")]
    worktree: bool,
    /// Show the comment's whole thread — root first, then every reply in
    /// time order — instead of just this comment. Conflicts with an
    /// `@<rev>` or `~N`/`^` suffix on `spec`, checked at runtime since the
    /// suffix lives inside `spec` rather than being its own flag.
    #[arg(long)]
    thread: bool,
}

fn main() -> Result<()> {
    // Install signal handlers before any lock is taken, so an interrupted
    // write cleans up its per-ref lock file (a gix_tempfile) instead of
    // leaving a stale one that wedges the ref. grace_count 0 → the first
    // SIGINT/SIGTERM cleans up and exits. (A SIGKILL or power loss can still
    // orphan a lock — nothing short of pid-aware lock breaking covers that.)
    //
    // SAFETY: the interrupt callback runs in a signal handler and does nothing
    // — no allocation, no locks — as required.
    #[allow(unsafe_code)]
    unsafe {
        gix::interrupt::init_handler(0, || {})?;
    }

    let cli = Cli::parse();
    let repo = gix::discover(".").context("not inside a git repository")?;

    // Handled before `Comments::open` borrows `repo`: `serve` needs to own
    // the repository, and no other command does.
    if matches!(cli.command.as_ref(), Some(Command::Lsp)) {
        return gix_comment_lsp::serve(repo).map_err(Into::into);
    }

    let comments = Comments::open(&repo);

    match cli.command {
        // Bare `git comment` lists, like `git remote` — a read-only default.
        None => cmd_list(&repo, &comments, None, false, false, false)?,
        Some(Command::Add(args)) => cmd_add(&repo, &comments, args)?,
        Some(Command::Reply(args)) => cmd_reply(&repo, &comments, args)?,
        Some(Command::Edit(args)) => cmd_edit(&repo, &comments, args)?,
        Some(Command::Append(args)) => cmd_append(&comments, args)?,
        Some(Command::Resolve { id }) => cmd_resolve(&comments, &id)?,
        Some(Command::Reopen { id }) => cmd_reopen(&comments, &id)?,
        Some(Command::List(args)) => {
            cmd_list(
                &repo,
                &comments,
                args.object,
                args.json,
                args.resolved,
                args.all,
            )?;
        }
        Some(Command::Show(args)) => {
            cmd_show(
                &repo,
                &comments,
                &args.spec,
                args.json,
                args.worktree,
                args.thread,
            )?;
        }
        Some(Command::Log { id }) => cmd_log(&comments, &id)?,
        Some(Command::Remove { ids }) => cmd_remove(&comments, &ids)?,
        Some(Command::Lsp) => unreachable!("handled above before `comments` borrowed `repo`"),
    }
    Ok(())
}

/// `add`: build the binding (a position, with `--path`, or the revision
/// itself), gather the message, resolve `--attach` if given, and attach.
fn cmd_add(repo: &gix::Repository, comments: &Comments<'_>, args: AddArgs) -> Result<()> {
    let AddArgs {
        object,
        path,
        lines,
        message,
        file,
        worktree,
        attach,
    } = args;

    // Reconcile `--path` with a path embedded in `-L START,END:path`: either
    // may supply it, but not two disagreeing values.
    let has_lines = lines.is_some();
    let lines_path = lines.as_ref().and_then(|l| l.path.clone());
    let raw_path = match (path, lines_path) {
        (Some(path), Some(lines_path)) if path != lines_path => {
            bail!("--path {path:?} and -L's embedded path {lines_path:?} disagree");
        }
        (Some(path), _) => Some(path),
        (None, Some(lines_path)) => Some(lines_path),
        (None, None) => None,
    };
    // clap can't express "requires `--path`, unless `-L`'s own value
    // supplies one" declaratively, so this is checked here — immediately,
    // before any git work — rather than via `requires = "path"`.
    if has_lines && raw_path.is_none() {
        bail!("-L/--lines requires --path (or a `:PATH` embedded in -L's value)");
    }
    let path = raw_path
        .map(|path| cwd_relative_path(repo, &path))
        .transpose()?;
    let range = lines.map(|l| l.range);

    let binding = if worktree {
        // clap's `requires = "path"` on `--worktree` guarantees this.
        let path = path.expect("--worktree requires --path");
        let anchor = capture_worktree(repo, &path, range)?;
        Binding::Position(anchor)
    } else {
        match path {
            Some(path) => {
                let object = object.unwrap_or_else(|| "HEAD".to_owned());
                let anchor = capture(repo, &object, &path, range)?;
                Binding::Position(anchor)
            }
            None => {
                let object = object.unwrap_or_else(|| "HEAD".to_owned());
                let commit = repo
                    .rev_parse_single(object.as_str())
                    .with_context(|| {
                        let mut msg = format!("cannot resolve revision {object:?}");
                        if Path::new(&object).exists() {
                            msg.push_str(&format!(
                                "; to anchor a file, use: git comment add --path {object}"
                            ));
                        }
                        msg
                    })?
                    .detach();
                Binding::Commit {
                    commit: commit.into(),
                }
            }
        }
    };

    let attachment = attach
        .as_deref()
        .map(|s| resolve_tree(repo, s))
        .transpose()?;

    let body = body_source(message.as_deref(), file.as_ref(), "")?;
    let id = comments.add(&binding, &String::from_utf8_lossy(&body), attachment)?;
    println!("{id}");
    Ok(())
}

/// `reply`: join an existing comment's thread with a new comment that
/// inherits its binding automatically.
fn cmd_reply(repo: &gix::Repository, comments: &Comments<'_>, args: ReplyArgs) -> Result<()> {
    let ReplyArgs {
        id,
        message,
        file,
        attach,
    } = args;
    let parent = resolve_comment(comments, &id)?;
    let attachment = attach
        .as_deref()
        .map(|s| resolve_tree(repo, s))
        .transpose()?;
    let body = body_source(message.as_deref(), file.as_ref(), "")?;
    let new_id = comments.reply(parent.id, &String::from_utf8_lossy(&body), attachment)?;
    println!("{new_id}");
    Ok(())
}

/// `resolve`: mark a comment resolved, message and attachment unchanged.
fn cmd_resolve(comments: &Comments<'_>, id: &str) -> Result<()> {
    let comment = resolve_comment(comments, id)?;
    let new_id = comments.resolve(comment.id)?;
    println!("{new_id}");
    Ok(())
}

/// `reopen`: mark a resolved comment open again.
fn cmd_reopen(comments: &Comments<'_>, id: &str) -> Result<()> {
    let comment = resolve_comment(comments, id)?;
    let new_id = comments.reopen(comment.id)?;
    println!("{new_id}");
    Ok(())
}

/// `edit`: reattach a comment's binding with a replacement message, seeding
/// the editor (when reached) with the comment's current message. `--attach`
/// replaces the attachment; omitted, the existing attachment (if any) is
/// kept.
fn cmd_edit(repo: &gix::Repository, comments: &Comments<'_>, args: EditArgs) -> Result<()> {
    let EditArgs {
        id,
        message,
        file,
        attach,
    } = args;
    let comment = resolve_comment(comments, &id)?;
    let body = body_source(message.as_deref(), file.as_ref(), &comment.message)?;
    let attachment = match attach {
        Some(s) => Some(resolve_tree(repo, &s)?),
        None => comment.attachment,
    };
    let new_id = comments.edit(comment.id, &String::from_utf8_lossy(&body), attachment)?;
    println!("{new_id}");
    Ok(())
}

/// `append`: reattach a comment's binding with new content joined onto the
/// existing message by a blank line, `git notes append` style. The
/// attachment (if any) is left unchanged.
fn cmd_append(comments: &Comments<'_>, args: AppendArgs) -> Result<()> {
    let AppendArgs { id, message, file } = args;
    let comment = resolve_comment(comments, &id)?;
    let addition = body_source(message.as_deref(), file.as_ref(), "")?;

    let mut new_message = comment.message.clone();
    if !new_message.is_empty() {
        new_message.push_str("\n\n");
    }
    new_message.push_str(&String::from_utf8_lossy(&addition));

    let new_id = comments.edit(comment.id, &new_message, comment.attachment)?;
    println!("{new_id}");
    Ok(())
}

/// `list`: thread roots, or only those attached to `<object>` — including a
/// position comment whose anchor's own commit is `<object>`, even though its
/// `target` (the anchored blob) is not. Open roots only by default;
/// `resolved` widens that to every root regardless of state; `all` drops the
/// roots-only filter entirely, listing every comment (roots and replies
/// alike, any state) — the flat view `list` gave before threads existed.
fn cmd_list(
    repo: &gix::Repository,
    comments: &Comments<'_>,
    object: Option<String>,
    json: bool,
    resolved: bool,
    all: bool,
) -> Result<()> {
    let target = match object {
        Some(object) => Some(
            repo.rev_parse_single(object.as_str())
                .with_context(|| format!("cannot resolve revision {object:?}"))?
                .detach(),
        ),
        None => None,
    };

    // Reply counts are computed off every comment regardless of `all`/
    // `resolved`/`target`, so a root's count always reflects its whole
    // thread, not just what this particular listing happens to show.
    let every_comment = comments.list(None)?;
    let reply_counts = reply_counts(&every_comment);

    let rows: Vec<Comment> = if all {
        every_comment
    } else {
        comments.list_roots(None, resolved)?
    };
    let rows: Vec<Comment> = match target {
        None => rows,
        Some(target) => rows
            .into_iter()
            .filter(|comment| {
                comment.target == target || position_commit(&comment.binding) == Some(target)
            })
            .collect(),
    };

    for comment in rows {
        let kind = binding_kind(&comment.binding);
        let replies = reply_counts.get(&comment.id).copied().unwrap_or(0);
        if json {
            print_list_json(&comment, kind, replies)?;
        } else {
            println!(
                "{}  {}  {}  {}{}",
                short(comment.id),
                comment.author.name,
                state_str(comment.state),
                first_line(&comment.message),
                reply_suffix(replies),
            );
        }
    }
    Ok(())
}

/// How many direct replies each comment (by id) has, counted across every
/// comment `list(None)` returns — [`cmd_list`]'s per-root reply count.
fn reply_counts(comments: &[Comment]) -> HashMap<ObjectId, usize> {
    let mut counts = HashMap::new();
    for comment in comments {
        if let Some(parent) = comment.parent {
            *counts.entry(parent).or_insert(0usize) += 1;
        }
    }
    counts
}

/// `" (N replies)"` for `count > 0`, else empty — [`cmd_list`]'s
/// human-readable reply-count suffix.
fn reply_suffix(count: usize) -> String {
    if count == 0 {
        String::new()
    } else {
        format!("  ({count} repl{})", if count == 1 { "y" } else { "ies" })
    }
}

/// A [`State`]'s porcelain label.
fn state_str(state: State) -> &'static str {
    match state {
        State::Open => "open",
        State::Resolved => "resolved",
    }
}

/// `show`: a comment's target, binding, author, message, state, and — for a
/// position — its anchored snippet. An `@<rev>` suffix projects the
/// comment's anchor onto `<rev>`; a `~N`/`^` suffix reads an older version of
/// the comment itself; `--worktree` projects onto the working tree;
/// `--thread` shows the comment's whole thread instead of just this comment
/// (and conflicts with either suffix, checked here since the suffix lives
/// inside `spec` rather than being its own flag clap can declare a conflict
/// against).
fn cmd_show(
    repo: &gix::Repository,
    comments: &Comments<'_>,
    spec: &str,
    json: bool,
    worktree: bool,
    thread: bool,
) -> Result<()> {
    let (id, selector) = split_show_spec(spec)?;
    let comment = resolve_comment(comments, id)?;

    if thread {
        if !matches!(selector, ShowSelector::Tip) {
            bail!("--thread conflicts with an @<rev> or ~N/^ suffix on the comment id");
        }
        return show_thread(comments, &comment, json);
    }

    match selector {
        ShowSelector::Projection(rev) => {
            if worktree {
                bail!("--worktree conflicts with an @<rev> suffix on the comment id");
            }
            show_projection(repo, &comment, rev, json)
        }
        ShowSelector::Ancestor(n) => {
            if worktree {
                bail!("--worktree conflicts with a ~N/^ suffix on the comment id");
            }
            let history = comments.history(comment.id)?;
            let commit = *history.get(n).ok_or_else(|| {
                anyhow::anyhow!(
                    "comment {} has {} version(s); ~{n} is out of range",
                    short(comment.id),
                    history.len()
                )
            })?;
            let versioned = comments.get_at(comment.id, commit)?;
            show_comment(&versioned, json)
        }
        ShowSelector::Tip if worktree => show_worktree(repo, &comment, json),
        ShowSelector::Tip => show_comment(&comment, json),
    }
}

/// `log`: a comment's version history, newest first — `<oid> <iso-date>
/// <author> <summary>` per version.
fn cmd_log(comments: &Comments<'_>, id: &str) -> Result<()> {
    let comment = resolve_comment(comments, id)?;
    for commit_id in comments.history(comment.id)? {
        let version = comments.get_at(comment.id, commit_id)?;
        let when = version
            .author
            .time
            .format(gix::date::time::format::ISO8601)?;
        println!(
            "{commit_id} {when} {} {}",
            version.author.name,
            first_line(&version.message)
        );
    }
    Ok(())
}

/// A comment at its captured location, human- or machine-readable.
fn show_comment(comment: &Comment, json: bool) -> Result<()> {
    let kind = binding_kind(&comment.binding);
    let anchor_snippet = match &comment.binding {
        Binding::Position(anchor) => Some(snippet(anchor)?),
        _ => None,
    };

    if json {
        print_comment_json(comment, kind, anchor_snippet.as_deref())?;
        return Ok(());
    }

    println!("id: {}", comment.id);
    println!("target: {}", comment.target);
    println!("binding: {kind}");
    println!("author: {} <{}>", comment.author.name, comment.author.email);
    println!(
        "date: {}",
        comment
            .author
            .time
            .format(gix::date::time::format::ISO8601)?
    );
    println!("state: {}", state_str(comment.state));
    if let Some(parent) = comment.parent {
        println!("parent: {parent}");
    }
    if let Some(attachment) = comment.attachment {
        println!("attachment: {attachment}");
    }
    println!("message:");
    println!("{}", comment.message);
    if let Some(text) = anchor_snippet {
        println!("snippet:");
        println!("{text}");
    }
    Ok(())
}

/// `show --thread`: the comment's whole thread, root first, then every reply
/// in time order.
fn show_thread(comments: &Comments<'_>, comment: &Comment, json: bool) -> Result<()> {
    let thread = comments.thread(comment.id)?;
    if json {
        print_thread_json(&thread)?;
        return Ok(());
    }
    print_thread_entry(&thread.root)?;
    for reply in &thread.replies {
        println!();
        print_thread_entry(reply)?;
    }
    Ok(())
}

/// One thread entry's human-readable form: author, date, state, and message
/// — [`show_thread`]'s per-comment block.
fn print_thread_entry(comment: &Comment) -> Result<()> {
    println!("id: {}", comment.id);
    println!("author: {} <{}>", comment.author.name, comment.author.email);
    println!(
        "date: {}",
        comment
            .author
            .time
            .format(gix::date::time::format::ISO8601)?
    );
    println!("state: {}", state_str(comment.state));
    println!("message:");
    println!("{}", comment.message);
    Ok(())
}

/// `show --thread --json`: one JSON object per line (root first, then every
/// reply), the same per-comment shape [`print_comment_json`] emits.
fn print_thread_json(thread: &Thread) -> Result<()> {
    print_comment_json(&thread.root, binding_kind(&thread.root.binding), None)?;
    for reply in &thread.replies {
        print_comment_json(reply, binding_kind(&reply.binding), None)?;
    }
    Ok(())
}

/// Re-derive where a position-bound comment's anchor now sits on `rev`.
fn show_projection(repo: &gix::Repository, comment: &Comment, rev: &str, json: bool) -> Result<()> {
    let Binding::Position(anchor) = &comment.binding else {
        bail!(
            "comment {} is a {} binding; @<rev> projection applies only to line/blob anchors (add --path)",
            short(comment.id),
            binding_kind(&comment.binding)
        );
    };
    let projection = project(repo, anchor, rev)?;
    print_projection(comment, &projection, anchor, json)
}

/// Re-derive where a position-bound comment's anchor now sits in the working
/// tree.
fn show_worktree(repo: &gix::Repository, comment: &Comment, json: bool) -> Result<()> {
    let Binding::Position(anchor) = &comment.binding else {
        bail!(
            "comment {} is a {} binding; --worktree projection applies only to line/blob anchors (add --path)",
            short(comment.id),
            binding_kind(&comment.binding)
        );
    };
    let projection = project_worktree(repo, anchor, None)?;
    print_projection(comment, &projection, anchor, json)
}

/// Print a projection outcome, human- or machine-readable — shared by
/// `show <id>@<rev>` and `show <id> --worktree`.
fn print_projection(
    comment: &Comment,
    projection: &Projection,
    anchor: &Anchor,
    json: bool,
) -> Result<()> {
    if json {
        print_projection_json(comment, projection);
        return Ok(());
    }
    println!("{}", projection.label());
    match projection {
        Projection::Relocated { path, lines } => {
            println!("path: {path}");
            if let Some(lines) = lines {
                println!("lines: {},{}", lines.start, lines.end);
            }
        }
        Projection::Current => println!("{}", snippet(anchor)?),
        Projection::Outdated { .. } | Projection::Deleted => {}
    }
    Ok(())
}

/// `remove`: delete every listed comment, having resolved all of them first
/// so an ambiguous or missing id leaves every comment untouched.
fn cmd_remove(comments: &Comments<'_>, ids: &[String]) -> Result<()> {
    let resolved: Vec<Comment> = ids
        .iter()
        .map(|id| resolve_comment(comments, id))
        .collect::<Result<_>>()?;
    for comment in &resolved {
        if !comments.remove(comment.id)? {
            bail!("no comment {}", comment.id);
        }
    }
    Ok(())
}

/// The comment message, in this precedence (mirroring `git notes add`): `-m
/// <msg>`, else `-F <file>`, else piped stdin, else — at a terminal with
/// neither — `$VISUAL`/`$EDITOR`, seeded with `seed`.
fn body_source(message: Option<&str>, file: Option<&PathBuf>, seed: &str) -> Result<Vec<u8>> {
    if let Some(message) = message {
        return Ok(message.as_bytes().to_vec());
    }
    if let Some(path) = file {
        return std::fs::read(path).with_context(|| format!("reading {}", path.display()));
    }
    if !std::io::stdin().is_terminal() {
        let mut buf = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buf)
            .context("reading stdin")?;
        return Ok(buf);
    }
    Ok(edit_in_editor(seed)?.into_bytes())
}

/// Open `$VISUAL`/`$EDITOR` on `seed` and return what the user saved.
fn edit_in_editor(seed: &str) -> Result<String> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_owned());

    let mut path = std::env::temp_dir();
    path.push(format!("git-comment-{}.txt", std::process::id()));
    std::fs::write(&path, seed).with_context(|| format!("writing {}", path.display()))?;

    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .with_context(|| format!("launching editor {editor:?}"))?;
    if !status.success() {
        let _ = std::fs::remove_file(&path);
        bail!("editor {editor:?} exited without saving");
    }

    let content = std::fs::read_to_string(&path)?;
    let _ = std::fs::remove_file(&path);
    Ok(content)
}

/// Resolve a tree-ish (`--attach`'s value) to the object id of its tree,
/// erroring clearly if it does not resolve to a tree.
fn resolve_tree(repo: &gix::Repository, spec: &str) -> Result<ObjectId> {
    let id = repo
        .rev_parse_single(spec)
        .with_context(|| format!("cannot resolve revision {spec:?}"))?
        .detach();
    let tree = repo
        .find_object(id)
        .with_context(|| format!("cannot read object {spec:?} ({id})"))?
        .peel_to_tree()
        .with_context(|| format!("{spec:?} ({id}) does not resolve to a tree"))?;
    Ok(tree.id().detach())
}

/// Resolve a comment id, accepting an unambiguous hex prefix. Errors when no
/// comment matches, or when more than one does.
fn resolve_comment(comments: &Comments<'_>, prefix: &str) -> Result<Comment> {
    let mut matches: Vec<Comment> = comments
        .list(None)?
        .into_iter()
        .filter(|comment| comment.id.to_string().starts_with(prefix))
        .collect();
    match matches.len() {
        0 => bail!("no comment matches id {prefix:?}"),
        1 => Ok(matches.remove(0)),
        n => bail!("id {prefix:?} is ambiguous: {n} comments match"),
    }
}

/// What a `show` argument's suffix selects, once the comment id prefix is
/// split off.
enum ShowSelector<'a> {
    /// No suffix: the comment's current tip.
    Tip,
    /// `<id>@<rev>`: project the position-bound anchor onto `<rev>`.
    Projection(&'a str),
    /// `<id>~N` (or bare `<id>~`, `<id>^`, meaning `~1`): the comment's
    /// message as of `N` versions back from the tip (`~0` is the tip
    /// itself).
    Ancestor(usize),
}

/// Split a `show` argument into a comment-id prefix and its suffix,
/// mirroring git's own revision grammar: `@<rev>` projects onto another
/// revision, `~N`/`^` walks the comment's own version history instead.
/// Comment ids are lowercase hex, so the first of `@`, `~`, `^` cleanly
/// separates the id from its suffix. `@{…}` (git's reflog/date syntax) is
/// rejected outright rather than mangled into a revision lookup that would
/// just fail confusingly downstream.
fn split_show_spec(spec: &str) -> Result<(&str, ShowSelector<'_>)> {
    let Some(i) = spec.find(['@', '~', '^']) else {
        return Ok((spec, ShowSelector::Tip));
    };
    let (id, marker) = spec.split_at(i);
    match marker.as_bytes()[0] {
        b'@' => {
            let rev = &marker[1..];
            if rev.starts_with('{') {
                bail!(
                    "{marker:?} looks like git's `@{{...}}` reflog syntax, which a comment id \
                     does not support; use `<id>@<rev>` to project onto a revision, or \
                     `<id>~N` to read an older version of the comment itself"
                );
            }
            if rev.is_empty() {
                Ok((id, ShowSelector::Tip))
            } else {
                Ok((id, ShowSelector::Projection(rev)))
            }
        }
        b'~' => {
            let rest = &marker[1..];
            let n: usize = if rest.is_empty() {
                1
            } else {
                rest.parse()
                    .map_err(|_error| anyhow::anyhow!("invalid version offset {marker:?}"))?
            };
            Ok((id, ShowSelector::Ancestor(n)))
        }
        b'^' => {
            if marker.len() > 1 {
                bail!("only a bare `^` is supported (no `^N`); use `~N` instead");
            }
            Ok((id, ShowSelector::Ancestor(1)))
        }
        _ => unreachable!("split only on '@', '~', or '^'"),
    }
}

/// `-L`'s value, once parsed: the range, and an optional path carried in a
/// trailing `:PATH` (`git log -L`'s own grammar).
#[derive(Debug, Clone)]
struct LinesArg {
    range: LineRange,
    path: Option<String>,
}

/// `-L`'s value: `start,end`, `start,+count`, or a single line number alone
/// standing in for `start,start` — optionally followed by `:path` to supply
/// the anchored path in the same token.
fn parse_lines_arg(raw: &str) -> std::result::Result<LinesArg, String> {
    let (range_part, path) = match raw.split_once(':') {
        Some((range_part, path)) => (range_part, Some(path.to_owned())),
        None => (raw, None),
    };

    let mut parts = range_part.splitn(2, ',');
    let start_str = parts.next().unwrap_or_default().trim();
    let end_str = parts.next().map(str::trim);
    let start: u64 = start_str
        .parse()
        .map_err(|_error| format!("invalid line number {start_str:?}"))?;
    let end = match end_str {
        None => start,
        Some(end_str) => match end_str.strip_prefix('+') {
            Some(count_str) => {
                let count: u64 = count_str
                    .parse()
                    .map_err(|_error| format!("invalid line count {count_str:?}"))?;
                start.saturating_add(count).saturating_sub(1).max(start)
            }
            None => end_str
                .parse()
                .map_err(|_error| format!("invalid line number {end_str:?}"))?,
        },
    };
    Ok(LinesArg {
        range: LineRange { start, end },
        path,
    })
}

/// Prefix a `--path` (or `-L`'s embedded path) value with the path from the
/// repository root to the current directory, so it behaves like an ordinary
/// git pathspec — resolved relative to cwd, not the repo root — the same
/// convention `git add <path>` uses.
fn cwd_relative_path(repo: &gix::Repository, path: &str) -> Result<String> {
    let prefix = repo
        .prefix()
        .context("determining the repository's cwd prefix")?;
    let mut parts: Vec<String> = Vec::new();
    if let Some(prefix) = prefix {
        parts.extend(path_components(prefix));
    }
    parts.extend(path_components(Path::new(path)));
    Ok(parts.join("/"))
}

/// The normal (non-`.`) path components of `path`, as plain strings —
/// `cwd_relative_path`'s helper, applied to both the repo prefix and the
/// user-supplied path so the joined result is a clean, forward-slash
/// pathspec regardless of the host platform's separator.
fn path_components(path: &Path) -> impl Iterator<Item = String> + '_ {
    path.components().filter_map(|component| match component {
        std::path::Component::CurDir => None,
        other => Some(other.as_os_str().to_string_lossy().into_owned()),
    })
}

/// A short, display-only prefix of an object id (not necessarily unique;
/// [`resolve_comment`] is the source of truth for id resolution).
fn short(id: ObjectId) -> String {
    id.to_string()[..8].to_owned()
}

/// The first line of a comment message.
fn first_line(message: &str) -> &str {
    message.lines().next().unwrap_or("")
}

/// This binding's porcelain kind name.
fn binding_kind(binding: &Binding) -> &'static str {
    match binding {
        Binding::Commit { .. } => "commit",
        Binding::Tree { .. } => "tree",
        Binding::Delta { .. } => "delta",
        Binding::Position(_) => "position",
        Binding::Hybrid { .. } => "hybrid",
    }
}

/// A position-bound binding's anchor's own commit, or `None` for any other
/// binding kind — `list <commit>`'s extra filter: a position comment's
/// `target` is the anchored blob, not the commit it was captured at, so
/// filtering on `target` alone would silently omit it.
fn position_commit(binding: &Binding) -> Option<ObjectId> {
    match binding {
        Binding::Position(anchor) => Some(anchor.commit()),
        _ => None,
    }
}

/// Emit `comment` as a small hand-formatted JSON object — kept
/// dependency-free rather than pulling in a JSON codec for one small, fixed
/// shape.
fn print_comment_json(comment: &Comment, kind: &str, snippet: Option<&str>) -> Result<()> {
    let time = comment
        .author
        .time
        .format(gix::date::time::format::ISO8601)?;
    let mut fields = vec![
        format!("\"id\":\"{}\"", comment.id),
        format!("\"target\":\"{}\"", comment.target),
        format!("\"binding\":\"{kind}\""),
        format!("\"author\":{}", json_string(&comment.author.name)),
        format!("\"email\":{}", json_string(&comment.author.email)),
        format!("\"time\":{}", json_string(&time)),
        format!("\"state\":\"{}\"", state_str(comment.state)),
        format!("\"message\":{}", json_string(&comment.message)),
    ];
    if let Some(parent) = comment.parent {
        fields.push(format!("\"parent\":\"{parent}\""));
    }
    if let Some(attachment) = comment.attachment {
        fields.push(format!("\"attachment\":\"{attachment}\""));
    }
    if let Some(snippet) = snippet {
        fields.push(format!("\"snippet\":{}", json_string(snippet)));
    }
    println!("{{{}}}", fields.join(","));
    Ok(())
}

/// Emit a `list` entry as a small JSON object: `id`, `target`, `binding`,
/// `author`, `email`, `time`, `state`, `replies` (this root's direct reply
/// count), `parent` (when this entry is itself a reply, e.g. under
/// `--all`), and `summary` (the message's first line).
fn print_list_json(comment: &Comment, kind: &str, replies: usize) -> Result<()> {
    let time = comment
        .author
        .time
        .format(gix::date::time::format::ISO8601)?;
    let mut fields = vec![
        format!("\"id\":\"{}\"", comment.id),
        format!("\"target\":\"{}\"", comment.target),
        format!("\"binding\":\"{kind}\""),
        format!("\"author\":{}", json_string(&comment.author.name)),
        format!("\"email\":{}", json_string(&comment.author.email)),
        format!("\"time\":{}", json_string(&time)),
        format!("\"state\":\"{}\"", state_str(comment.state)),
        format!("\"replies\":{replies}"),
        format!("\"summary\":{}", json_string(first_line(&comment.message))),
    ];
    if let Some(parent) = comment.parent {
        fields.push(format!("\"parent\":\"{parent}\""));
    }
    println!("{{{}}}", fields.join(","));
    Ok(())
}

/// Emit a projection outcome as a small JSON object: the comment's own `id`
/// and `target` (so the object is self-describing on its own), its
/// `outcome`, plus the `path` (and `lines`, when known) for a relocated or
/// outdated span.
fn print_projection_json(comment: &Comment, projection: &Projection) {
    let mut fields = vec![
        format!("\"id\":\"{}\"", comment.id),
        format!("\"target\":\"{}\"", comment.target),
        format!("\"outcome\":\"{}\"", projection.label()),
    ];
    match projection {
        Projection::Relocated { path, lines } => {
            fields.push(format!("\"path\":{}", json_string(path)));
            if let Some(lines) = lines {
                fields.push(format!(
                    "\"lines\":{{\"start\":{},\"end\":{}}}",
                    lines.start, lines.end
                ));
            }
        }
        Projection::Outdated { path } => fields.push(format!("\"path\":{}", json_string(path))),
        Projection::Current | Projection::Deleted => {}
    }
    println!("{{{}}}", fields.join(","));
}

/// A minimal JSON string literal: quotes, backslashes, and control
/// characters escaped; everything else passed through verbatim.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
