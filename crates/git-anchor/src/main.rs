//! `git-anchor`: a git external subcommand (`git anchor …`) that captures
//! bindings and injects them into entities of any kind published in a
//! `gix-store` registry — it defines no document type of its own.
//!
//! `inject <kind>` requires `<kind>`'s published schema to embed
//! [`gix_anchor::Binding`]'s shape (located by structural comparison, not by
//! name): that field is always filled from a previously [`create`](Command::Create)d
//! binding, never from user text. `list`/`show`/`remove` work the same way
//! for any kind, published schema or not, read back as [`facet_value::Value`]
//! rather than a compiled Rust type.
//!
//! `show` prints an entity as stored and nothing more: the pin-free oracle
//! chain (`diff_trace`/`fingerprint_oracle`/`op_log`) is library-internal
//! (ARCHITECTURE.md: "`project` is library-internal... no user-facing
//! command resolves through it") — resolving a binding onto another
//! revision is `git-query`'s `bind/5`, not this CLI's job.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use facet_git_tree::{
    Node, ObjectStore, Schema, StructField, deserialize_value_with_schema, schema_of,
};
use facet_value::{VObject, Value};
use gix_anchor::{
    Binding, CaptureHandle, CommitIdentity, LineRange, NoHints, capture, capture_worktree,
};
use gix_store::{
    DocumentBuilder, Layout, RefPath, RefPrefix, RefSegment, RepoStore, entity_name_under,
};

#[derive(Parser)]
#[command(
    name = "git-anchor",
    about = "Capture bindings and inject them into entities of any registered kind",
    version
)]
struct Cli {
    /// The store's ref namespace. Defaults to `gix-anchor`'s own; pass any
    /// other prefix a `gix-store` consumer publishes under.
    #[arg(long, global = true, default_value = "refs/anchors")]
    prefix: RefPrefix,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Capture a binding, writing its identity and hints objects. Advances
    /// no ref and needs no registered kind. Prints the capture handle —
    /// pass it to `inject --anchor` or to `id`.
    Create(CreateArgs),
    /// Print a capture handle's anchor id: the `identity` subtree's oid,
    /// invariant under any hint change.
    Id(IdArgs),
    /// Write an entity of `<kind>` embedding a previously created binding.
    /// `<kind>` must be anchorable (its published schema embeds `Binding`'s
    /// shape) — this is `git anchor`'s reason to exist.
    Inject(InjectArgs),
    /// List every entity of `<kind>`.
    #[command(visible_alias = "ls")]
    List(ListArgs),
    /// Show one entity of `<kind>` by its full entity name (as `list` or
    /// `inject` printed it), exactly as stored.
    Show(ShowArgs),
    /// Remove one or more entities of `<kind>`, by full entity name. Every
    /// name is checked to exist before any entity is removed.
    #[command(visible_alias = "rm")]
    Remove(RemoveArgs),
}

/// Arguments shared by capture: the revision/path/lines a binding names.
#[derive(clap::Args)]
struct CreateArgs {
    /// The revision the binding names (whole-commit), or that `--path`/`-L`
    /// resolve a blob against (position). Defaults to `HEAD`. Conflicts
    /// with `--worktree`.
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
}

/// Arguments for `id`.
#[derive(clap::Args)]
struct IdArgs {
    /// A capture handle, as printed by `git anchor create`.
    handle: CaptureHandle,
}

/// Arguments for `inject`.
#[derive(clap::Args)]
struct InjectArgs {
    /// The kind to write an entity of; must be anchorable.
    kind: RefSegment,
    /// Fills the kind's one remaining required `String` field. Conflicts
    /// with `--json`, which supplies the whole document instead.
    #[arg(conflicts_with = "json")]
    text: Option<String>,
    /// A previously created capture handle, as printed by `git anchor
    /// create`.
    #[arg(long, value_name = "HANDLE")]
    anchor: CaptureHandle,
    /// A whole `facet_value::Value` JSON literal for the document — the
    /// escape hatch when no single positional argument can fill the kind's
    /// remaining required fields. The binding field is still injected from
    /// `--anchor`, overriding anything this literal sets there.
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
    /// An entity's full name.
    name: String,
    /// Emit the entity as a single JSON value instead of the human-readable
    /// form.
    #[arg(long)]
    json: bool,
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
    // write releases its per-ref lock file instead of wedging the ref.
    // SAFETY: the callback runs in a signal handler and does nothing.
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
        Some(Command::Create(args)) => cmd_create(&repo, args)?,
        Some(Command::Id(args)) => cmd_id(&repo, args)?,
        Some(Command::Inject(args)) => cmd_inject(&repo, &store, args)?,
        Some(Command::List(args)) => cmd_list(&store, &args.kind, args.json)?,
        Some(Command::Show(args)) => cmd_show(&store, args)?,
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

/// `create`: capture a binding and write it to the repository's object
/// database. Advances no ref; prints the capture handle, content-addressed
/// over identity *and* hints, so capturing the same coordinates against the
/// same repository state twice prints the same handle.
fn cmd_create(repo: &gix::Repository, args: CreateArgs) -> Result<()> {
    let CreateArgs {
        at,
        path,
        lines,
        worktree,
    } = args;
    let binding = build_binding(repo, at, path, lines, worktree)?;
    let handle = binding.serialize_into(repo)?;
    println!("{handle}");
    Ok(())
}

/// `id`: the anchor id a capture handle resolves to — `identity`'s own oid,
/// read directly off the handle's tree, invariant under any hint change.
fn cmd_id(repo: &gix::Repository, args: IdArgs) -> Result<()> {
    let id = args.handle.anchor_id(repo)?;
    println!("{id}");
    Ok(())
}

/// `inject`: locate the binding field by reflection, fill it from
/// `--anchor`, fill the one remaining required `String` field (if any) from
/// `<text>`, default every `Optional` field to absent, and refuse if
/// anything required is still unfilled — unless `--json` supplied the whole
/// document, in which case only the binding field is ever overridden.
fn cmd_inject(repo: &gix::Repository, store: &RepoStore<'_>, args: InjectArgs) -> Result<()> {
    let InjectArgs {
        kind,
        text,
        anchor,
        json,
    } = args;

    let dynamic = store.dynamic(kind.clone());
    let schema = dynamic
        .schema()
        .get()?
        .ok_or_else(|| anyhow!("no schema published for kind {kind}"))?;
    let binding_field_name = binding_field(&schema)?;
    let fields = struct_fields(&schema)?;

    let binding =
        Binding::deserialize(&anchor, repo).with_context(|| format!("reading binding {anchor}"))?;

    // Every unset `Optional`-shaped field defaults to absent; a real value
    // supplied below (json, text, or the binding) simply overwrites it.
    let mut builder = DocumentBuilder::for_schema(&schema)?;
    for (name, field) in fields {
        if !field.has_default && matches!(resolve(&schema, &field.node), Some(Node::Optional(_))) {
            builder.set(name, Value::NULL)?;
        }
    }

    if let Some(raw) = &json {
        let value: Value = facet_json::from_str(raw).context("parsing --json value")?;
        let obj = value
            .as_object()
            .ok_or_else(|| anyhow!("--json value must be a JSON object"))?;
        for (name, value) in obj.iter() {
            builder.set(name.as_str(), value.clone())?;
        }
    } else if let Some(text) = &text {
        let field = unique_string_field(&schema, fields, &binding_field_name)
            .with_context(|| format!("kind {kind} does not accept a positional argument"))?;
        builder.set(&field, text.as_str())?;
    }
    builder.set(&binding_field_name, binding_to_value(&binding)?)?;

    let value = builder
        .build()
        .with_context(|| format!("kind {kind}: fill remaining required field(s) with --json"))?;

    let anchor_id = binding.anchor_id()?;
    let group = RefPath::from(RefSegment::new(anchor_id.to_string()).expect("hex oid is valid"));
    let message = format!("{kind} {anchor_id}");
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

/// `show`: an entity exactly as stored. Resolving a position binding onto
/// another revision is `git-query`'s `bind/5`, not this CLI's job
/// (ARCHITECTURE.md: "`project` is library-internal").
fn cmd_show(store: &RepoStore<'_>, args: ShowArgs) -> Result<()> {
    let ShowArgs { kind, name, json } = args;
    let name = RefPath::new(&name).with_context(|| format!("invalid entity name {name:?}"))?;

    let dynamic = store.dynamic(kind.clone());
    let value = dynamic
        .get(&name)?
        .ok_or_else(|| anyhow!("no entity {name} in kind {kind}"))?;
    print_value(&name, &value, json)
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

/// Build the [`Binding`] `create` captures: a position (`--path`/`-L`, at a
/// revision or, with `--worktree`, on-disk) or the bare revision itself,
/// defaulting to `HEAD`.
fn build_binding(
    repo: &gix::Repository,
    at: Option<String>,
    path: Option<String>,
    lines: Option<LinesArg>,
    worktree: bool,
) -> Result<Binding> {
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
                identity: CommitIdentity {
                    commit: commit.into(),
                },
                hints: NoHints {},
            })
        }
    }
}

/// Encode `binding` as a [`Value`] conforming to its own schema.
///
/// Not `facet_value::to_value`: its generic serializer has no notion of
/// `facet-git-tree`'s byte-sequence leaf (`Oid`'s `#[facet(transparent)]`
/// `[u8; 20]`), so it emits a plain number array that then fails to write
/// under a schema expecting `Node::Bytes`. Round-tripping through the tree
/// codec instead sidesteps the gap.
fn binding_to_value(binding: &Binding) -> Result<Value> {
    let store = ObjectStore::default();
    let root = facet_git_tree::serialize_into(binding, &store).context("encoding the binding")?;
    let schema = schema_of::<Binding>().context("Binding's own schema")?;
    deserialize_value_with_schema(&root, &schema, &store).context("re-reading the binding")
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

/// The one field of `schema`'s root struct whose shape structurally equals
/// [`Binding`]'s own. Refuses when zero or more than one field matches.
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
            join(many)
        ),
    }
}

/// The one field, among `fields` excluding `binding_field`, that is required
/// (not `Optional`, no default) and shaped `Node::String` — `inject`'s
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
            join(many)
        ),
    }
}

/// `names`, comma-joined.
fn join(names: &[&String]) -> String {
    names
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(", ")
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

/// Resolve `--path` (or `-L`'s embedded path) relative to cwd, like any git
/// pathspec — the same convention `git add <path>` uses.
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

/// `path`'s normal (non-`.`) components, as plain strings.
fn path_components(path: &Path) -> impl Iterator<Item = String> + '_ {
    path.components().filter_map(|component| match component {
        std::path::Component::CurDir => None,
        other => Some(other.as_os_str().to_string_lossy().into_owned()),
    })
}
