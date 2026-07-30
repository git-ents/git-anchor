//! `git-anchor`: a git external subcommand (`git anchor …`) that writes,
//! reads, and removes entities of any kind published in a `gix-store`
//! registry — it defines no document type of its own.
//!
//! `add <kind>` requires `<kind>`'s published schema to embed
//! [`gix_anchor::Binding`]'s shape (located by structural comparison, not by
//! name): that field is always filled from the capture pipeline
//! (`--at`/`--path`/`-L`/`--worktree`), never from user text. The one other
//! remaining required field whose shape is `String` is filled from a
//! positional argument when exactly one such field exists; otherwise, and for
//! any kind whose document shape does not reduce this cleanly, `--json`
//! supplies the whole document literally. `list`/`show`/`remove` work the
//! same way for any kind, published schema or not read back as
//! [`facet_value::Value`] rather than a compiled Rust type. `show`'s
//! `@<rev>`/`--worktree` projection re-derives where a position binding sits
//! elsewhere, exactly as it always did — it operates on the [`Binding`]
//! extracted from the read entity, not on any document-specific field.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use facet_git_tree::{
    Node, ObjectStore, Schema, StructField, deserialize_value_with_schema, schema_of,
    serialize_value_with_schema,
};
use facet_value::{VObject, Value};
use gix_anchor::{
    Anchor, Binding, LineRange, Projection, capture, capture_worktree, project, project_worktree,
    snippet,
};
use gix_store::{Layout, RefPath, RefPrefix, RefSegment, RepoStore, entity_name_under};

#[derive(Parser)]
#[command(
    name = "git-anchor",
    about = "Write entities of any registered kind",
    version
)]
struct Cli {
    /// The store's ref namespace. Defaults to `gix-anchor`'s own; pass
    /// `refs/comments` to reach `gix-comment`'s kinds, or any other prefix a
    /// `gix-store` consumer publishes under.
    #[arg(long, global = true, default_value = "refs/anchors")]
    prefix: RefPrefix,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Add an entity of `<kind>`. `<kind>` must be anchorable (its published
    /// schema embeds `Binding`'s shape) — this is `git anchor`'s reason to
    /// exist, not an incidental restriction.
    Add(AddArgs),
    /// List every entity of `<kind>`.
    #[command(visible_alias = "ls")]
    List(ListArgs),
    /// Show one entity of `<kind>` by its full entity name (as `list` or
    /// `add` printed it). `@<rev>` projects a position binding onto another
    /// revision; `--worktree` projects onto the working tree.
    Show(ShowArgs),
    /// Remove one or more entities of `<kind>`, by full entity name. Every
    /// name is checked to exist before any entity is removed.
    #[command(visible_alias = "rm")]
    Remove(RemoveArgs),
}

/// Arguments for `add`.
#[derive(clap::Args)]
struct AddArgs {
    /// The kind to add an entity of.
    kind: RefSegment,
    /// Fills the kind's one remaining required `String` field. Conflicts
    /// with `--json`, which supplies the whole document instead.
    #[arg(conflicts_with = "json")]
    text: Option<String>,
    /// The revision the binding names (`Binding::Commit`), or that
    /// `--path`/`-L` resolve a blob against (`Binding::Position`). Defaults
    /// to `HEAD`. Conflicts with `--worktree`.
    #[arg(long, value_name = "REV", conflicts_with = "worktree")]
    at: Option<String>,
    /// Anchor a specific blob path instead of the revision itself. Resolved
    /// relative to the current directory, like any git pathspec.
    #[arg(long = "path", value_name = "PATH")]
    path: Option<String>,
    /// A 1-based inclusive line range: `start,end`, `start,+count`, or a
    /// single line number. A trailing `:path` supplies `--path` in the same
    /// token. Requires a path from one source or the other.
    #[arg(
        short = 'L',
        long = "lines",
        value_name = "START,END[:PATH]",
        value_parser = parse_lines_arg
    )]
    lines: Option<LinesArg>,
    /// Anchor `--path`'s on-disk content instead of a revision; conflicts
    /// with `--at`.
    #[arg(long, requires = "path", conflicts_with = "at")]
    worktree: bool,
    /// A whole `facet_value::Value` JSON literal for the document — the
    /// escape hatch when no single positional argument can fill the kind's
    /// remaining required fields. The binding field is still injected from
    /// the capture pipeline, overriding anything this literal sets there.
    #[arg(long, value_name = "VALUE")]
    json: Option<String>,
}

/// Arguments for `list`.
#[derive(clap::Args)]
struct ListArgs {
    /// The kind to list.
    kind: RefSegment,
    /// Emit one JSON object (`{"name": ..., "value": ...}`) per line.
    #[arg(long)]
    json: bool,
}

/// Arguments for `show`.
#[derive(clap::Args)]
struct ShowArgs {
    /// The kind to read from.
    kind: RefSegment,
    /// An entity's full name, optionally with an `@<rev>` suffix.
    spec: String,
    /// Emit the entity as a single JSON value instead of the human-readable
    /// form.
    #[arg(long)]
    json: bool,
    /// Project onto the working tree instead of showing the entity as
    /// stored. Conflicts with an `@<rev>` suffix on `spec`.
    #[arg(long)]
    worktree: bool,
}

/// Arguments for `remove`.
#[derive(clap::Args)]
struct RemoveArgs {
    /// The kind to remove from.
    kind: RefSegment,
    /// One or more entities' full names.
    names: Vec<RefPath>,
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
    let layout = Layout {
        data: cli.prefix.child(&segment("data")),
        schema: cli.prefix.child(&segment("schema")),
    };
    let store = RepoStore::open_with_layout(&repo, layout);

    match cli.command {
        None => cmd_kinds(&store)?,
        Some(Command::Add(args)) => cmd_add(&repo, &store, args)?,
        Some(Command::List(args)) => cmd_list(&store, &args.kind, args.json)?,
        Some(Command::Show(args)) => cmd_show(&repo, &store, args)?,
        Some(Command::Remove(args)) => cmd_remove(&store, &args.kind, &args.names)?,
    }
    Ok(())
}

fn segment(value: &str) -> RefSegment {
    RefSegment::new(value).expect("built-in ref segment is valid")
}

/// Bare `git anchor`: every kind with a published schema, marking which are
/// anchorable.
fn cmd_kinds(store: &RepoStore<'_>) -> Result<()> {
    for kind in store.kinds()? {
        let anchorable = store
            .dynamic(kind.clone())
            .schema()
            .get()?
            .is_some_and(|schema| binding_field(&schema).is_ok());
        if anchorable {
            println!("{kind}  (anchorable)");
        } else {
            println!("{kind}");
        }
    }
    Ok(())
}

/// `add`: locate the binding field by reflection, fill it from the capture
/// pipeline, fill the one remaining required `String` field (if any) from
/// `<text>`, default every `Optional` field to absent, and refuse if
/// anything required is still unfilled — unless `--json` supplied the whole
/// document, in which case only the binding field is ever overridden.
fn cmd_add(repo: &gix::Repository, store: &RepoStore<'_>, args: AddArgs) -> Result<()> {
    let AddArgs {
        kind,
        text,
        at,
        path,
        lines,
        worktree,
        json,
    } = args;

    let dynamic = store.dynamic(kind.clone());
    let schema = dynamic
        .schema()
        .get()?
        .ok_or_else(|| anyhow!("no schema published for kind {kind}"))?;
    let binding_field_name = binding_field(&schema)?;
    let fields = struct_fields(&schema)?;

    let binding = build_binding(repo, at, path, lines, worktree)?;
    let target = binding.target();

    let mut value: Value = match &json {
        Some(raw) => facet_json::from_str(raw).context("parsing --json value")?,
        None => VObject::new().into(),
    };
    let obj = value
        .as_object_mut()
        .ok_or_else(|| anyhow!("--json value must be a JSON object"))?;
    obj.insert(binding_field_name.clone(), binding_to_value(&binding)?);

    if json.is_none() {
        if let Some(text) = &text {
            let field = unique_string_field(&schema, fields, &binding_field_name)
                .with_context(|| format!("kind {kind} does not accept a positional argument"))?;
            obj.insert(field, Value::from(text.as_str()));
        }
        for (name, field) in fields {
            if name == &binding_field_name || obj.contains_key(name.as_str()) || field.has_default {
                continue;
            }
            if matches!(resolve(&schema, &field.node), Some(Node::Optional(_))) {
                obj.insert(name.clone(), Value::NULL);
            }
        }
        let missing: Vec<&str> = fields
            .iter()
            .filter(|(name, field)| {
                name.as_str() != binding_field_name
                    && !field.has_default
                    && !obj.contains_key(name.as_str())
            })
            .map(|(name, _)| name.as_str())
            .collect();
        if !missing.is_empty() {
            bail!(
                "kind {kind} has required field(s) with no way to fill from the command \
                 line: {}; supply the whole document with --json",
                missing.join(", ")
            );
        }
    }

    let group = RefPath::from(RefSegment::new(target.to_string()).expect("hex oid is valid"));
    let message = format!("{kind} {target}");
    let commit = dynamic
        .write(&value)
        .message(message)
        .anonymous_under(&group)?;
    println!("{}", entity_name_under(&group, commit));
    Ok(())
}

/// `list`: every entity of `<kind>`, name plus value.
fn cmd_list(store: &RepoStore<'_>, kind: &RefSegment, json: bool) -> Result<()> {
    let dynamic = store.dynamic(kind.clone());
    for name in dynamic.list()? {
        let Some(value) = dynamic.get(&name)? else {
            continue;
        };
        if json {
            let mut obj = VObject::new();
            obj.insert("name", name.to_string());
            obj.insert("value", value);
            println!("{}", facet_json::to_string(&Value::from(obj))?);
        } else {
            println!("{name}  {}", facet_json::to_string(&value)?);
        }
    }
    Ok(())
}

/// `show`: an entity as stored, or — with an `@<rev>` suffix or
/// `--worktree` — its position binding projected elsewhere.
fn cmd_show(repo: &gix::Repository, store: &RepoStore<'_>, args: ShowArgs) -> Result<()> {
    let ShowArgs {
        kind,
        spec,
        json,
        worktree,
    } = args;
    let (name_str, rev) = split_show_spec(&spec)?;
    if worktree && rev.is_some() {
        bail!("--worktree conflicts with an @<rev> suffix on the entity name");
    }
    let name =
        RefPath::new(name_str).with_context(|| format!("invalid entity name {name_str:?}"))?;

    let dynamic = store.dynamic(kind.clone());
    let value = dynamic
        .get(&name)?
        .ok_or_else(|| anyhow!("no entity {name} in kind {kind}"))?;

    if rev.is_none() && !worktree {
        print_value(&name, &value, json)?;
        return Ok(());
    }

    let schema = dynamic
        .schema()
        .get()?
        .ok_or_else(|| anyhow!("no schema published for kind {kind}"))?;
    let field = binding_field(&schema)?;
    let binding_value = value
        .as_object()
        .and_then(|obj| obj.get(&field))
        .cloned()
        .ok_or_else(|| anyhow!("entity {name} has no {field:?} field"))?;
    let binding = value_to_binding(&binding_value)?;
    let Binding::Position(anchor) = binding else {
        bail!(
            "entity {name} is a {} binding; @<rev>/--worktree projection applies only to \
             position bindings",
            binding_kind(&binding)
        );
    };
    let projection = match rev {
        Some(rev) => project(repo, &anchor, rev)?,
        None => project_worktree(repo, &anchor, None)?,
    };
    print_projection(&name, &projection, &anchor, json)
}

/// `remove`: delete every named entity, having confirmed all of them exist
/// first, so a missing name leaves every entity untouched.
fn cmd_remove(store: &RepoStore<'_>, kind: &RefSegment, names: &[RefPath]) -> Result<()> {
    let dynamic = store.dynamic(kind.clone());
    for name in names {
        if dynamic.get(name)?.is_none() {
            bail!("no entity {name} in kind {kind}");
        }
    }
    for name in names {
        dynamic.remove(name)?;
    }
    Ok(())
}

/// Build the [`Binding`] `add` injects: a position (`--path`/`-L`, at a
/// revision or, with `--worktree`, on-disk) or the bare revision itself,
/// defaulting to `HEAD`.
fn build_binding(
    repo: &gix::Repository,
    at: Option<String>,
    path: Option<String>,
    lines: Option<LinesArg>,
    worktree: bool,
) -> Result<Binding> {
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
    if has_lines && raw_path.is_none() {
        bail!("-L/--lines requires --path (or a `:PATH` embedded in -L's value)");
    }
    let path = raw_path
        .map(|path| cwd_relative_path(repo, &path))
        .transpose()?;
    let range = lines.map(|l| l.range);

    if worktree {
        // clap's `requires = "path"` on `--worktree` guarantees this.
        let path = path.expect("--worktree requires --path");
        let anchor = capture_worktree(repo, &path, range)?;
        return Ok(Binding::Position(anchor));
    }
    match path {
        Some(path) => {
            let at = at.unwrap_or_else(|| "HEAD".to_owned());
            let anchor = capture(repo, &at, &path, range)?;
            Ok(Binding::Position(anchor))
        }
        None => {
            let at = at.unwrap_or_else(|| "HEAD".to_owned());
            let commit = repo
                .rev_parse_single(at.as_str())
                .with_context(|| {
                    let mut msg = format!("cannot resolve revision {at:?}");
                    if Path::new(&at).exists() {
                        msg.push_str(&format!("; to anchor a file, use: --path {at}"));
                    }
                    msg
                })?
                .detach();
            Ok(Binding::Commit {
                commit: commit.into(),
            })
        }
    }
}

/// Encode `binding` as a [`Value`] conforming to its own schema.
///
/// Not `facet_value::to_value`: that goes through `facet-format`'s generic
/// event serializer, which has no notion of `facet-git-tree`'s byte-sequence
/// leaf (`Oid`'s `[u8; 20]`, `#[facet(transparent)]`) and emits a plain
/// array of numbers for it — a `Value` that then fails to write under a
/// schema whose matching field is `Node::Bytes`. Round-tripping through the
/// tree codec instead — the same encode/decode pair every stored entity
/// already goes through — sidesteps that gap entirely: the write side
/// already handles byte sequences correctly, and reading the result back
/// with the schema in hand yields exactly the `Value` a schema-conformant
/// write needs, in memory, no repository involved.
fn binding_to_value(binding: &Binding) -> Result<Value> {
    let store = ObjectStore::default();
    let root = facet_git_tree::serialize_into(binding, &store).context("encoding the binding")?;
    let schema = schema_of::<Binding>().context("Binding's own schema")?;
    deserialize_value_with_schema(&root, &schema, &store).context("re-reading the binding")
}

/// The inverse of [`binding_to_value`]: decode a [`Value`] already known to
/// conform to `Binding`'s schema (it came off a schema-directed read) back
/// into a real [`Binding`]. Not `facet_value::from_value`, for the same
/// reason `to_value` is unusable on the way in — its generic deserializer
/// expects a `[u8; N]`-shaped field to arrive as a JSON-style array, not the
/// `Value::Bytes` a schema-directed read actually produces for one. Writing
/// the value back out under the schema (which does understand
/// `Node::Bytes`) and reading the result with the ordinary typed decoder
/// sidesteps the gap the same way.
fn value_to_binding(value: &Value) -> Result<Binding> {
    let store = ObjectStore::default();
    let schema = schema_of::<Binding>().context("Binding's own schema")?;
    let root = serialize_value_with_schema(value, &schema, &store)
        .context("re-encoding the binding field")?;
    facet_git_tree::deserialize(&root, &store).context("decoding the binding field")
}

/// The one field of `schema`'s root struct whose shape structurally equals
/// [`Binding`]'s own — `DEVPLAN-boundary.md`'s "Locating the binding field by
/// reflection". Refuses when zero or more than one field matches.
fn binding_field(schema: &Schema) -> Result<String> {
    let canonical = schema_of::<Binding>().context("Binding's own schema")?;
    let canonical_root =
        resolve(&canonical, &canonical.root).context("Binding's root does not resolve")?;
    let fields = struct_fields(schema)?;
    let matches: Vec<&String> = fields
        .iter()
        .filter(|(_, field)| resolve(schema, &field.node) == Some(canonical_root))
        .map(|(name, _)| name)
        .collect();
    match matches.as_slice() {
        [name] => Ok((*name).clone()),
        [] => bail!("not anchorable: no field in its schema matches Binding's shape"),
        many => bail!(
            "ambiguously anchorable: fields {} all match Binding's shape",
            many.iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// `schema`'s root, resolved to its struct fields.
fn struct_fields(schema: &Schema) -> Result<&BTreeMap<String, StructField>> {
    match resolve(schema, &schema.root) {
        Some(Node::Struct(fields)) => Ok(fields),
        _ => bail!("kind's schema does not describe a struct document"),
    }
}

/// One [`Node::Ref`] indirection into `schema.defs`, or the node itself when
/// it is not a `Ref`.
fn resolve<'s>(schema: &'s Schema, node: &'s Node) -> Option<&'s Node> {
    match node {
        Node::Ref(name) => schema.defs.get(name),
        other => Some(other),
    }
}

/// The one field, among `fields` excluding `binding_field`, that is required
/// (not `Optional`, no default) and shaped `Node::String` — `add`'s
/// positional argument fills exactly this field, when exactly one exists.
fn unique_string_field(
    schema: &Schema,
    fields: &BTreeMap<String, StructField>,
    binding_field: &str,
) -> Result<String> {
    let candidates: Vec<&String> = fields
        .iter()
        .filter(|(name, field)| {
            name.as_str() != binding_field
                && !field.has_default
                && !matches!(resolve(schema, &field.node), Some(Node::Optional(_)))
                && matches!(resolve(schema, &field.node), Some(Node::String))
        })
        .map(|(name, _)| name)
        .collect();
    match candidates.as_slice() {
        [name] => Ok((*name).clone()),
        [] => bail!("no required String field to fill (besides the binding field)"),
        many => bail!(
            "ambiguous: {} candidate String fields ({})",
            many.len(),
            many.iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// An entity as stored, human- or machine-readable.
fn print_value(name: &RefPath, value: &Value, json: bool) -> Result<()> {
    if json {
        println!("{}", facet_json::to_string(value)?);
    } else {
        println!("name: {name}");
        println!("{}", facet_json::to_string_pretty(value)?);
    }
    Ok(())
}

/// A projection outcome, human- or machine-readable — shared by `show
/// <kind> <name>@<rev>` and `show <kind> <name> --worktree`.
fn print_projection(
    name: &RefPath,
    projection: &Projection,
    anchor: &Anchor,
    json: bool,
) -> Result<()> {
    if json {
        let mut obj = VObject::new();
        obj.insert("name", name.to_string());
        obj.insert("outcome", projection.label().to_string());
        match projection {
            Projection::Relocated { path, lines } => {
                obj.insert("path", path.as_str());
                if let Some(lines) = lines {
                    let mut l = VObject::new();
                    l.insert("start", i64::try_from(lines.start).unwrap_or(i64::MAX));
                    l.insert("end", i64::try_from(lines.end).unwrap_or(i64::MAX));
                    obj.insert("lines", l);
                }
            }
            Projection::Outdated { path } => {
                obj.insert("path", path.as_str());
            }
            Projection::Current | Projection::Deleted => {}
        }
        println!("{}", facet_json::to_string(&Value::from(obj))?);
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

/// Split a `show` argument into an entity-name prefix and an optional
/// `@<rev>` suffix. Entity names this crate mints are hex-oid path segments,
/// which never contain `@`, so a plain split is exact. `@{…}` (git's
/// reflog/date syntax) is rejected outright rather than mangled into a
/// revision lookup that would just fail confusingly downstream.
fn split_show_spec(spec: &str) -> Result<(&str, Option<&str>)> {
    let Some((name, rev)) = spec.split_once('@') else {
        return Ok((spec, None));
    };
    if rev.starts_with('{') {
        bail!(
            "{spec:?} looks like git's `@{{...}}` reflog syntax, which an entity name does \
             not support; use `<name>@<rev>` to project onto a revision"
        );
    }
    if rev.is_empty() {
        Ok((name, None))
    } else {
        Ok((name, Some(rev)))
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
