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
//! revision. `remove` deletes a note.

use std::io::{IsTerminal, Read};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use gix::ObjectId;
use gix_anchor::{Binding, LineRange, Projection, Store, StoredNote, capture, project, snippet};

#[derive(Parser)]
#[command(name = "git-anchor", about = "Attach content to Git objects", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Attach a note to a revision (defaults to `HEAD`), or, with `--path`,
    /// to a specific blob path and optional line range within it.
    Add(AddArgs),
    /// List attached notes, or only those attached to `<object>`.
    #[command(visible_alias = "ls")]
    List {
        /// A revision to filter notes down to those attached to it.
        object: Option<String>,
    },
    /// Show a note's target, binding, and body. Append `@<rev>` to a
    /// position-bound note's id to project it onto another commit instead,
    /// re-deriving where its anchor now sits.
    Show {
        /// A note id (an unambiguous hex prefix is fine), optionally with an
        /// `@<rev>` suffix to project onto that revision.
        spec: String,
        /// Emit a machine-readable object instead of the human-readable form.
        #[arg(long)]
        json: bool,
    },
    /// Remove a note.
    #[command(visible_alias = "rm")]
    Remove {
        /// A note id, or an unambiguous hex prefix of one.
        id: String,
    },
}

/// Arguments for `add`.
#[derive(clap::Args)]
struct AddArgs {
    /// The revision to attach to. Defaults to `HEAD`.
    object: Option<String>,
    /// Anchor a specific blob path (as it exists at `<object>`) instead of
    /// the revision itself.
    #[arg(long = "path", value_name = "PATH")]
    path: Option<String>,
    /// Anchor a line range within `--path`: `start,end` (1-based,
    /// inclusive), or a single line number alone. Requires `--path`.
    #[arg(short = 'L', long = "lines", value_name = "START,END", value_parser = parse_line_range)]
    lines: Option<LineRange>,
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
        Command::Add(args) => cmd_add(&repo, &store, args)?,
        Command::List { object } => cmd_list(&repo, &store, object)?,
        Command::Show { spec, json } => cmd_show(&repo, &store, &spec, json)?,
        Command::Remove { id } => cmd_remove(&store, &id)?,
    }
    Ok(())
}

/// `add`: build the binding (a position, with `--path`, or the revision
/// itself), gather the body, and attach it.
fn cmd_add(repo: &gix::Repository, store: &Store, args: AddArgs) -> Result<()> {
    let AddArgs {
        object,
        path,
        lines,
        message,
        file,
    } = args;
    let object = object.unwrap_or_else(|| "HEAD".to_owned());

    let binding = match &path {
        Some(path) => {
            let anchor = capture(repo, &object, path, lines)?;
            Binding::Position(anchor)
        }
        None => {
            if lines.is_some() {
                bail!("-L/--lines requires --path");
            }
            let commit = repo
                .rev_parse_single(object.as_str())
                .with_context(|| format!("cannot resolve revision {object:?}"))?
                .detach();
            Binding::Commit { commit }
        }
    };

    let body = body_source(message.as_deref(), file.as_ref())?;
    let id = store.attach(&binding, &body, None)?;
    println!("{id}");
    Ok(())
}

/// `list`: every note, or only those attached to `<object>`.
fn cmd_list(repo: &gix::Repository, store: &Store, object: Option<String>) -> Result<()> {
    let target = match object {
        Some(object) => Some(
            repo.rev_parse_single(object.as_str())
                .with_context(|| format!("cannot resolve revision {object:?}"))?
                .detach(),
        ),
        None => None,
    };
    for note in store.list(target)? {
        println!(
            "{}  {}  {}",
            short(note.id),
            short(note.target),
            first_line(&note.body)
        );
    }
    Ok(())
}

/// `show`: a note's target, binding, body, and — for a position — its
/// anchored snippet. With an `@<rev>` suffix on the id, project the note's
/// anchor onto `<rev>` instead of showing its captured location.
fn cmd_show(repo: &gix::Repository, store: &Store, spec: &str, json: bool) -> Result<()> {
    let (id, rev) = split_id_rev(spec);
    let note = resolve_note(store, id)?;
    match rev {
        Some(rev) => show_projection(repo, &note, rev, json),
        None => show_note(&note, json),
    }
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
    if !note.message.is_empty() {
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
    if json {
        print_projection_json(&projection);
        return Ok(());
    }
    println!("{}", projection.label());
    match &projection {
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

/// `remove`: delete a note.
fn cmd_remove(store: &Store, id: &str) -> Result<()> {
    let note = resolve_note(store, id)?;
    if !store.remove(note.id)? {
        bail!("no note {}", note.id);
    }
    Ok(())
}

/// The note body, in this precedence (mirroring `git notes add`): `-m
/// <msg>`, else `-F <file>`, else piped stdin, else — at a terminal with
/// neither — `$VISUAL`/`$EDITOR`.
fn body_source(message: Option<&str>, file: Option<&PathBuf>) -> Result<Vec<u8>> {
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
    Ok(edit_in_editor("")?.into_bytes())
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
fn resolve_note(store: &Store, prefix: &str) -> Result<StoredNote> {
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

/// Split a `show` argument into a note-id prefix and an optional `@<rev>`
/// projection target. Note ids are lowercase hex, so the first `@` cleanly
/// separates the id from the revision; a bare trailing `@` carries no rev.
fn split_id_rev(spec: &str) -> (&str, Option<&str>) {
    match spec.split_once('@') {
        Some((id, rev)) if !rev.is_empty() => (id, Some(rev)),
        Some((id, _)) => (id, None),
        None => (spec, None),
    }
}

/// `-L`'s value: `start,end`, or a single line number standing in for
/// `start,start`.
fn parse_line_range(raw: &str) -> std::result::Result<LineRange, String> {
    let mut parts = raw.splitn(2, ',');
    let start = parts.next().unwrap_or_default().trim();
    let end = parts.next().map(str::trim).unwrap_or(start);
    let start: u64 = start
        .parse()
        .map_err(|_error| format!("invalid line number {start:?}"))?;
    let end: u64 = end
        .parse()
        .map_err(|_error| format!("invalid line number {end:?}"))?;
    Ok(LineRange { start, end })
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

/// Emit a projection outcome as a small JSON object: always its `outcome`,
/// plus the `path` (and `lines`, when known) for a relocated or outdated span.
fn print_projection_json(projection: &Projection) {
    let mut fields = vec![format!("\"outcome\":\"{}\"", projection.label())];
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
