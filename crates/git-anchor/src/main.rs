//! `git-anchor`: a git external subcommand (`git anchor …`) that attaches
//! arbitrary content to Git objects, either the objects themselves (a
//! commit) or a durable position within one (a line range in a blob at a
//! commit) — the [`gix_anchor::Binding`] vocabulary, driven from the shell.
//!
//! `add` attaches a note: to a bare revision (`Binding::Commit`) or, with
//! `--path`, to a specific blob path and optional line range
//! (`Binding::Position`, a [`gix_anchor::Anchor`]). `list` and `show` read
//! notes back — `show <id>@<rev>` projects a position-bound note onto another
//! commit, re-deriving where its anchor now sits, the way git addresses a
//! revision; `show <id>~N` reads an older version of the note itself. `edit`
//! and `append` reattach a new or extended body; `log` prints a note's
//! version history. `remove` deletes one or more notes. Bare `git anchor`
//! lists, like `git remote`.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use gix::ObjectId;
use gix_anchor::{
    Anchor, Binding, LineRange, Projection, RepoStore, Store, StoredNote, capture,
    capture_worktree, project, project_worktree, snippet,
};

#[derive(Parser)]
#[command(name = "git-anchor", about = "Attach content to Git objects", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Attach a note to a revision (defaults to `HEAD`), or, with `--path`,
    /// to a specific blob path and optional line range within it.
    Add(AddArgs),
    /// Replace a note's body. With no `-m`/`-F` and nothing piped, opens
    /// `$EDITOR` seeded with the current body.
    Edit(EditArgs),
    /// Append to a note's body, separated by a blank line — the new content
    /// is gathered the same way `add`'s body is.
    Append(EditArgs),
    /// List attached notes, or only those attached to `<object>`.
    #[command(visible_alias = "ls")]
    List(ListArgs),
    /// Show a note's target, binding, and body. Append `@<rev>` to a
    /// position-bound note's id to project it onto another commit instead,
    /// re-deriving where its anchor now sits; append `~N` or `^` to see an
    /// older version of the note itself.
    Show(ShowArgs),
    /// Show a note's version history, newest first.
    Log {
        /// A note id (an unambiguous hex prefix is fine).
        id: String,
    },
    /// Remove one or more notes.
    #[command(visible_alias = "rm")]
    Remove {
        /// One or more note ids (unambiguous hex prefixes are fine). Every
        /// id is resolved before any note is removed, so an ambiguous or
        /// missing id leaves all notes untouched.
        ids: Vec<String>,
    },
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
    /// The note body, taken verbatim.
    #[arg(
        short = 'm',
        long = "message",
        value_name = "MSG",
        conflicts_with = "file"
    )]
    message: Option<String>,
    /// Read the note body from a file.
    #[arg(short = 'F', long = "file", value_name = "FILE")]
    file: Option<PathBuf>,
    /// Anchor the working tree's on-disk content at `--path` instead of a
    /// committed revision. Requires `--path`; conflicts with `<object>`.
    #[arg(long, requires = "path", conflicts_with = "object")]
    worktree: bool,
}

/// Arguments shared by `edit` and `append`.
#[derive(clap::Args)]
struct EditArgs {
    /// A note id, or an unambiguous hex prefix of one.
    id: String,
    /// The new (or, for `append`, additional) body, taken verbatim.
    #[arg(
        short = 'm',
        long = "message",
        value_name = "MSG",
        conflicts_with = "file"
    )]
    message: Option<String>,
    /// Read the body from a file.
    #[arg(short = 'F', long = "file", value_name = "FILE")]
    file: Option<PathBuf>,
}

/// Arguments for `list`.
#[derive(clap::Args)]
struct ListArgs {
    /// A revision to filter notes down to those attached to it (including a
    /// position note whose anchor was captured at that commit).
    object: Option<String>,
    /// Emit one JSON object per line instead of the human-readable columns.
    #[arg(long)]
    json: bool,
}

/// Arguments for `show`.
#[derive(clap::Args)]
struct ShowArgs {
    /// A note id (an unambiguous hex prefix is fine), optionally with an
    /// `@<rev>` suffix to project onto that revision, or a `~N`/`^` suffix
    /// to read an older version of the note.
    spec: String,
    /// Emit a machine-readable object instead of the human-readable form.
    #[arg(long)]
    json: bool,
    /// Project onto the working tree instead of showing the captured
    /// location. Conflicts with an `@<rev>` or `~N`/`^` suffix on `spec`.
    #[arg(long)]
    worktree: bool,
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
    let store = Store::open(&repo);

    match cli.command {
        // Bare `git anchor` lists, like `git remote` — a read-only default.
        None => cmd_list(&repo, &store, None, false)?,
        Some(Command::Add(args)) => cmd_add(&repo, &store, args)?,
        Some(Command::Edit(args)) => cmd_edit(&store, args)?,
        Some(Command::Append(args)) => cmd_append(&store, args)?,
        Some(Command::List(args)) => cmd_list(&repo, &store, args.object, args.json)?,
        Some(Command::Show(args)) => cmd_show(&repo, &store, &args.spec, args.json, args.worktree)?,
        Some(Command::Log { id }) => cmd_log(&repo, &store, &id)?,
        Some(Command::Remove { ids }) => cmd_remove(&store, &ids)?,
    }
    Ok(())
}

/// `add`: build the binding (a position, with `--path`, or the revision
/// itself), gather the body, and attach it.
fn cmd_add(repo: &gix::Repository, store: &RepoStore<'_>, args: AddArgs) -> Result<()> {
    let AddArgs {
        object,
        path,
        lines,
        message,
        file,
        worktree,
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
                                "; to anchor a file, use: git anchor add --path {object}"
                            ));
                        }
                        msg
                    })?
                    .detach();
                Binding::Commit { commit }
            }
        }
    };

    let body = body_source(message.as_deref(), file.as_ref(), "")?;
    let id = store.attach(&binding, &body, None)?;
    println!("{id}");
    Ok(())
}

/// `edit`: reattach a note's binding with a replacement body, seeding the
/// editor (when reached) with the note's current body.
fn cmd_edit(store: &RepoStore<'_>, args: EditArgs) -> Result<()> {
    let EditArgs { id, message, file } = args;
    let note = resolve_note(store, &id)?;
    let seed = String::from_utf8_lossy(&note.body).into_owned();
    let body = body_source(message.as_deref(), file.as_ref(), &seed)?;
    let new_id = store.attach(&note.binding, &body, None)?;
    println!("{new_id}");
    Ok(())
}

/// `append`: reattach a note's binding with new content joined onto the
/// existing body by a blank line, `git notes append` style.
fn cmd_append(store: &RepoStore<'_>, args: EditArgs) -> Result<()> {
    let EditArgs { id, message, file } = args;
    let note = resolve_note(store, &id)?;
    let addition = body_source(message.as_deref(), file.as_ref(), "")?;

    let mut body = note.body.clone();
    if !body.is_empty() {
        body.extend_from_slice(b"\n\n");
    }
    body.extend_from_slice(&addition);

    let new_id = store.attach(&note.binding, &body, None)?;
    println!("{new_id}");
    Ok(())
}

/// `list`: every note, or only those attached to `<object>` — including a
/// position note whose anchor's own commit is `<object>`, even though its
/// `target` (the anchored blob) is not.
fn cmd_list(
    repo: &gix::Repository,
    store: &RepoStore<'_>,
    object: Option<String>,
    json: bool,
) -> Result<()> {
    let target = match object {
        Some(object) => Some(
            repo.rev_parse_single(object.as_str())
                .with_context(|| format!("cannot resolve revision {object:?}"))?
                .detach(),
        ),
        None => None,
    };

    let notes = store.list(None)?;
    let notes: Vec<StoredNote> = match target {
        None => notes,
        Some(target) => notes
            .into_iter()
            .filter(|note| note.target == target || position_commit(&note.binding) == Some(target))
            .collect(),
    };

    for note in notes {
        let kind = binding_kind(&note.binding);
        if json {
            print_note_json(&note, kind);
        } else {
            println!(
                "{}  {}  {}",
                short(note.id),
                short(note.target),
                first_line(&note.body)
            );
        }
    }
    Ok(())
}

/// `show`: a note's target, binding, body, and — for a position — its
/// anchored snippet. An `@<rev>` suffix projects the note's anchor onto
/// `<rev>`; a `~N`/`^` suffix reads an older version of the note itself;
/// `--worktree` projects onto the working tree.
fn cmd_show(
    repo: &gix::Repository,
    store: &RepoStore<'_>,
    spec: &str,
    json: bool,
    worktree: bool,
) -> Result<()> {
    let (id, selector) = split_show_spec(spec)?;
    let note = resolve_note(store, id)?;
    match selector {
        ShowSelector::Projection(rev) => {
            if worktree {
                bail!("--worktree conflicts with an @<rev> suffix on the note id");
            }
            show_projection(repo, &note, rev, json)
        }
        ShowSelector::Ancestor(n) => {
            if worktree {
                bail!("--worktree conflicts with a ~N/^ suffix on the note id");
            }
            let history = store.history(note.id)?;
            let commit = *history.get(n).ok_or_else(|| {
                anyhow::anyhow!(
                    "note {} has {} version(s); ~{n} is out of range",
                    short(note.id),
                    history.len()
                )
            })?;
            let versioned = store.get_at(note.id, commit)?;
            show_note(&versioned, json)
        }
        ShowSelector::Tip if worktree => show_worktree(repo, &note, json),
        ShowSelector::Tip => show_note(&note, json),
    }
}

/// `log`: a note's version history, newest first — `<oid> <iso-date>
/// <summary>` per version.
fn cmd_log(repo: &gix::Repository, store: &RepoStore<'_>, id: &str) -> Result<()> {
    let note = resolve_note(store, id)?;
    for commit_id in store.history(note.id)? {
        let commit = repo.find_commit(commit_id)?;
        let when = commit.time()?.format(gix::date::time::format::ISO8601)?;
        let summary = gix::objs::commit::MessageRef::from_bytes(commit.message_raw_sloppy())
            .summary()
            .to_string();
        println!("{commit_id} {when} {summary}");
    }
    Ok(())
}

/// A note at its captured location, human- or machine-readable.
fn show_note(note: &StoredNote, json: bool) -> Result<()> {
    let kind = binding_kind(&note.binding);
    let anchor_snippet = match &note.binding {
        Binding::Position(anchor) => Some(snippet(anchor)?),
        _ => None,
    };

    if json {
        print_json(note, kind, anchor_snippet.as_deref());
        return Ok(());
    }

    println!("id: {}", note.id);
    println!("target: {}", note.target);
    println!("binding: {kind}");
    // The auto-generated `anchor <target>` summary (`Store::attach`'s
    // default when no message is given) is storage plumbing, not something
    // the user wrote — suppress it here; `--json` still reports it, and a
    // real summary now surfaces via `log`.
    let default_message = format!("anchor {}", note.target);
    if !note.message.is_empty() && note.message != default_message {
        println!("message: {}", note.message);
    }
    println!("body:");
    println!("{}", String::from_utf8_lossy(&note.body));
    if let Some(text) = anchor_snippet {
        println!("snippet:");
        println!("{text}");
    }
    Ok(())
}

/// Re-derive where a position-bound note's anchor now sits on `rev`.
fn show_projection(repo: &gix::Repository, note: &StoredNote, rev: &str, json: bool) -> Result<()> {
    let Binding::Position(anchor) = &note.binding else {
        bail!(
            "note {} is a {} binding; @<rev> projection applies only to line/blob anchors (add --path)",
            short(note.id),
            binding_kind(&note.binding)
        );
    };
    let projection = project(repo, anchor, rev)?;
    print_projection(note, &projection, anchor, json)
}

/// Re-derive where a position-bound note's anchor now sits in the working
/// tree.
fn show_worktree(repo: &gix::Repository, note: &StoredNote, json: bool) -> Result<()> {
    let Binding::Position(anchor) = &note.binding else {
        bail!(
            "note {} is a {} binding; --worktree projection applies only to line/blob anchors (add --path)",
            short(note.id),
            binding_kind(&note.binding)
        );
    };
    let projection = project_worktree(repo, anchor, None)?;
    print_projection(note, &projection, anchor, json)
}

/// Print a projection outcome, human- or machine-readable — shared by
/// `show <id>@<rev>` and `show <id> --worktree`.
fn print_projection(
    note: &StoredNote,
    projection: &Projection,
    anchor: &Anchor,
    json: bool,
) -> Result<()> {
    if json {
        print_projection_json(note, projection);
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

/// `remove`: delete every listed note, having resolved all of them first so
/// an ambiguous or missing id leaves every note untouched.
fn cmd_remove(store: &RepoStore<'_>, ids: &[String]) -> Result<()> {
    let notes: Vec<StoredNote> = ids
        .iter()
        .map(|id| resolve_note(store, id))
        .collect::<Result<_>>()?;
    for note in &notes {
        if !store.remove(note.id)? {
            bail!("no note {}", note.id);
        }
    }
    Ok(())
}

/// The note body, in this precedence (mirroring `git notes add`): `-m
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
    path.push(format!("git-anchor-{}.txt", std::process::id()));
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

/// Resolve a note id, accepting an unambiguous hex prefix. Errors when no
/// note matches, or when more than one does.
fn resolve_note(store: &RepoStore<'_>, prefix: &str) -> Result<StoredNote> {
    let mut matches: Vec<StoredNote> = store
        .list(None)?
        .into_iter()
        .filter(|note| note.id.to_string().starts_with(prefix))
        .collect();
    match matches.len() {
        0 => bail!("no note matches id {prefix:?}"),
        1 => Ok(matches.remove(0)),
        n => bail!("id {prefix:?} is ambiguous: {n} notes match"),
    }
}

/// What a `show` argument's suffix selects, once the note id prefix is split
/// off.
enum ShowSelector<'a> {
    /// No suffix: the note's current tip.
    Tip,
    /// `<id>@<rev>`: project the position-bound anchor onto `<rev>`.
    Projection(&'a str),
    /// `<id>~N` (or bare `<id>~`, `<id>^`, meaning `~1`): the note's body as
    /// of `N` versions back from the tip (`~0` is the tip itself).
    Ancestor(usize),
}

/// Split a `show` argument into a note-id prefix and its suffix, mirroring
/// git's own revision grammar: `@<rev>` projects onto another revision,
/// `~N`/`^` walks the note's own version history instead. Note ids are
/// lowercase hex, so the first of `@`, `~`, `^` cleanly separates the id
/// from its suffix. `@{…}` (git's reflog/date syntax) is rejected outright
/// rather than mangled into a revision lookup that would just fail
/// confusingly downstream.
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
                    "{marker:?} looks like git's `@{{...}}` reflog syntax, which a note id \
                     does not support; use `<id>@<rev>` to project onto a revision, or \
                     `<id>~N` to read an older version of the note itself"
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
/// [`resolve_note`] is the source of truth for id resolution).
fn short(id: ObjectId) -> String {
    id.to_string()[..8].to_owned()
}

/// The first line of a note body, decoded lossily.
fn first_line(body: &[u8]) -> String {
    String::from_utf8_lossy(body)
        .lines()
        .next()
        .unwrap_or("")
        .to_owned()
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
/// binding kind — `list <commit>`'s extra filter (item 4): a position note's
/// `target` is the anchored blob, not the commit it was captured at, so
/// filtering on `target` alone would silently omit it.
fn position_commit(binding: &Binding) -> Option<ObjectId> {
    match binding {
        Binding::Position(anchor) => Some(anchor.commit()),
        _ => None,
    }
}

/// Emit `note` as a small hand-formatted JSON object — kept dependency-free
/// rather than pulling in a JSON codec for one small, fixed shape.
fn print_json(note: &StoredNote, kind: &str, snippet: Option<&str>) {
    let mut fields = vec![
        format!("\"id\":\"{}\"", note.id),
        format!("\"target\":\"{}\"", note.target),
        format!("\"binding\":\"{kind}\""),
        format!("\"message\":{}", json_string(&note.message)),
        format!(
            "\"body\":{}",
            json_string(&String::from_utf8_lossy(&note.body))
        ),
    ];
    if let Binding::Position(anchor) = &note.binding {
        fields.push(format!("\"path\":{}", json_string(&anchor.path)));
        if let Some(lines) = anchor.lines {
            fields.push(format!(
                "\"lines\":{{\"start\":{},\"end\":{}}}",
                lines.start, lines.end
            ));
        }
    }
    if let Some(snippet) = snippet {
        fields.push(format!("\"snippet\":{}", json_string(snippet)));
    }
    println!("{{{}}}", fields.join(","));
}

/// Emit a `list` entry as a small JSON object: `id`, `target`, `binding`,
/// and `summary` (the latest version's commit summary).
fn print_note_json(note: &StoredNote, kind: &str) {
    let fields = [
        format!("\"id\":\"{}\"", note.id),
        format!("\"target\":\"{}\"", note.target),
        format!("\"binding\":\"{kind}\""),
        format!("\"summary\":{}", json_string(&note.message)),
    ];
    println!("{{{}}}", fields.join(","));
}

/// Emit a projection outcome as a small JSON object: the note's own `id` and
/// `target` (so the object is self-describing on its own, item 11), its
/// `outcome`, plus the `path` (and `lines`, when known) for a relocated or
/// outdated span.
fn print_projection_json(note: &StoredNote, projection: &Projection) {
    let mut fields = vec![
        format!("\"id\":\"{}\"", note.id),
        format!("\"target\":\"{}\"", note.target),
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
