//! `git-attest`: a git external subcommand (`git attest …`) over
//! [`gix_attest`]'s claim chains — five subcommands, exactly the five
//! `ARCHITECTURE.md` names:
//!
//! ```text
//! git attest sign    <target> <payload-tree>
//! git attest revoke  <claim-id>
//! git attest verify  <claim-id>          # crypto only
//! git attest log     <target>
//! git attest resolve <target>
//! ```
//!
//! Targets are written `<kind>:<hex>` — `anchor:7f3e` is
//! `Target { kind: "anchor", id: 7f3e }`. The kind is a label this CLI never
//! interprets and never checks against a list: anchor vocabulary is not a
//! dependency of attest, and an allow-list of kinds here would make it one.
//!
//! Two things this binary owns that the library deliberately does not:
//!
//! - **signing**, because the *choice* of key is a user interface concern.
//!   [`signer`] shells out to `ssh-keygen -Y sign -n git`, so the block stored
//!   in the commit's `gpgsig` header is what git's own ssh backend would have
//!   written and `git verify-commit` accepts it with none of our tooling
//!   installed.
//! - **payload documents**, when `--json`/`--interactive` build one instead of
//!   `<payload-tree>` naming an existing tree. That write goes through
//!   `gix-store`'s [`DocumentBuilder`] and its dynamic kind handle — the
//!   dynamic write path exists once, and this is a caller of it.
//!
//! `verify` reports cryptography and nothing else: a [`Verdict::Verified`]
//! claim may still be revoked, may be signed by a key nobody trusts, and may
//! be inadmissible under every rule that will ever read it. The wording and the
//! exit code both say so.

// No crate-level `forbid(unsafe_code)`, for the one reason `git-anchor`'s
// binary has none either: installing git's signal handler so an interrupted
// write releases its ref lock is an `unsafe` call, and it is worth making.
// Every library in this workspace forbids it.

mod signer;

use std::io::BufRead as _;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use facet_git_tree::{Node, Schema};
use facet_value::{VObject, Value};
use gix::ObjectId;
use gix_attest::{
    AttestKey, Claim, Claims, Envelope, KEY_KIND, KEY_TARGET_KIND, Target, Verdict,
    register_schemas,
};
use gix_store::{DocumentBuilder, Layout, RefPrefix, RefSegment, RepoStore};

use crate::signer::SigningKey;

#[derive(Parser)]
#[command(
    name = "git-attest",
    about = "Sign, revoke, and read claims: signed envelopes chained on claim refs",
    version
)]
struct Cli {
    /// The claim store's ref namespace: claim refs live at
    /// `<prefix>/claims/<target-key>` and schemas at `<prefix>/schema/*`. The
    /// default puts claims at `refs/claims/<target-key>`, the specified layout.
    #[arg(long, global = true, default_value = "refs")]
    prefix: RefPrefix,
    /// The ssh private key to sign with. Defaults to git's own configuration:
    /// `user.signingkey`, with `gpg.format = ssh`.
    #[arg(long, global = true, value_name = "PATH")]
    signing_key: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Sign a claim about `<target>` and append it to that target's chain.
    /// Prints the claim id — the claim commit's own oid.
    Sign(SignArgs),
    /// Revoke `<claim-id>`: a claim about that claim, appended to its own
    /// chain, so reading the chain shows the revocation. Prints the
    /// revocation's claim id.
    Revoke(RevokeArgs),
    /// Report the *cryptographic* verdict on `<claim-id>` and nothing else.
    /// Exits 0 for a sound signature, 1 for an unsound one, 2 when nothing
    /// could be checked. A sound signature is not a valid claim.
    Verify(VerifyArgs),
    /// The chain of claims about `<target>`, newest first, exactly as written.
    Log(TargetArgs),
    /// `log` with revocations applied structurally: the same chain, with every
    /// revoked claim marked.
    Resolve(TargetArgs),
}

/// Arguments for `sign`.
#[derive(clap::Args)]
struct SignArgs {
    /// What the claim is about, as `<kind>:<hex>`. The kind is any label a
    /// vocabulary owner publishes and is never interpreted here.
    target: TargetArg,
    /// The payload's store tree hash — carried, never fetched. Omit it to
    /// build the payload document instead, with `--json` or `--interactive`.
    #[arg(value_name = "PAYLOAD-TREE")]
    payload_tree: Option<String>,
    /// The store schema kind the payload was written under: a label consumers
    /// join against `<prefix>/schema/<kind>`.
    #[arg(long, value_name = "KIND")]
    kind: RefSegment,
    /// Build the payload document for `--kind` from this whole
    /// `facet_value::Value` JSON literal, writing it as an entity of that kind
    /// and claiming the tree it compiles to.
    #[arg(long, value_name = "VALUE", conflicts_with = "payload_tree")]
    json: Option<String>,
    /// Build the payload document for `--kind` by prompting for each field its
    /// published schema names, one answer per line.
    #[arg(
        short = 'i',
        long,
        conflicts_with_all = ["payload_tree", "json"]
    )]
    interactive: bool,
    /// The key-add (or rotate) claim naming the signing key. Defaults to the
    /// claim publishing `--signing-key`'s public key, adding it if the store
    /// has never seen it.
    #[arg(long, value_name = "CLAIM-ID")]
    key: Option<String>,
    /// Publish the signing key as a machine actor's rather than a human's.
    /// Recorded on the key document, never interpreted here.
    #[arg(long)]
    machine: bool,
}

/// Arguments for `revoke`.
#[derive(clap::Args)]
struct RevokeArgs {
    /// The claim to revoke.
    #[arg(value_name = "CLAIM-ID")]
    claim: String,
    /// The key-add claim naming the signing key, as `sign --key`.
    #[arg(long, value_name = "CLAIM-ID")]
    key: Option<String>,
    /// As `sign --machine`.
    #[arg(long)]
    machine: bool,
}

/// Arguments for `verify`.
#[derive(clap::Args)]
struct VerifyArgs {
    /// The claim to check the signature of.
    #[arg(value_name = "CLAIM-ID")]
    claim: String,
    /// Emit the verdict as a single JSON object instead of the report.
    #[arg(long)]
    json: bool,
}

/// Arguments for `log` and `resolve`.
#[derive(clap::Args)]
struct TargetArgs {
    /// The target whose chain to read, as `<kind>:<hex>`.
    target: TargetArg,
    /// Emit one JSON object per claim, newest first.
    #[arg(long)]
    json: bool,
}

/// A target as written on the command line: `<kind>:<hex>`.
///
/// The hex half is resolved against the repository only once one is open, so a
/// short prefix works exactly as it does anywhere else in git; the kind half is
/// carried through untouched.
#[derive(Debug, Clone)]
struct TargetArg {
    kind: String,
    id: String,
}

impl std::str::FromStr for TargetArg {
    type Err = String;

    fn from_str(raw: &str) -> std::result::Result<Self, Self::Err> {
        // `rsplit_once`: a kind label may not contain a colon, and the hash
        // never does, so the last colon is the separator either way.
        let Some((kind, id)) = raw.rsplit_once(':') else {
            return Err(format!("expected <kind>:<hex>, got {raw:?}"));
        };
        if kind.is_empty() || id.is_empty() {
            return Err(format!("expected <kind>:<hex>, got {raw:?}"));
        }
        Ok(TargetArg {
            kind: kind.to_owned(),
            id: id.to_owned(),
        })
    }
}

fn main() -> Result<ExitCode> {
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
        data: cli.prefix.clone(),
        schema: cli.prefix.child(&segment("schema")),
    };

    match cli.command {
        Command::Sign(args) => {
            let key = SigningKey::resolve(&repo, cli.signing_key.as_deref())?;
            let store = RepoStore::open_with_layout(&repo, layout).with_signer(key.signer());
            cmd_sign(&repo, &store, &key, args)?;
        }
        Command::Revoke(args) => {
            let key = SigningKey::resolve(&repo, cli.signing_key.as_deref())?;
            let store = RepoStore::open_with_layout(&repo, layout).with_signer(key.signer());
            cmd_revoke(&repo, &store, &key, args)?;
        }
        Command::Verify(args) => {
            let store = RepoStore::open_with_layout(&repo, layout);
            return cmd_verify(&repo, &store, args);
        }
        Command::Log(args) => {
            let store = RepoStore::open_with_layout(&repo, layout);
            let target = target(&repo, &args.target)?;
            let claims: Vec<Claim> = Claims::open(&store).log(&target)?.collect();
            print_claims(&claims, args.json)?;
        }
        Command::Resolve(args) => {
            let store = RepoStore::open_with_layout(&repo, layout);
            let target = target(&repo, &args.target)?;
            let claims = Claims::open(&store).resolve(&target)?;
            print_claims(&claims, args.json)?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn segment(value: &str) -> RefSegment {
    RefSegment::new(value).expect("built-in ref segment is valid")
}

/// `sign`: resolve the signing key to a key claim, obtain the payload tree —
/// named, or built through store's dynamic write path — and append the claim.
fn cmd_sign(
    repo: &gix::Repository,
    store: &RepoStore<'_>,
    key: &SigningKey,
    args: SignArgs,
) -> Result<()> {
    let SignArgs {
        target: target_arg,
        payload_tree,
        kind,
        json,
        interactive,
        key: key_claim_arg,
        machine,
    } = args;

    ensure_schemas(store)?;
    let claims = Claims::open(store);
    let key_claim = key_claim(repo, store, key, key_claim_arg.as_deref(), machine)?;

    let payload = match (&payload_tree, &json, interactive) {
        (Some(hex), _, _) => object(repo, hex)?,
        (None, Some(raw), _) => write_payload(store, &kind, parse_json(raw)?)?,
        (None, None, true) => {
            let value = prompt_document(store, &kind)?;
            write_payload(store, &kind, value)?
        }
        (None, None, false) => bail!(
            "no payload: name a <payload-tree>, or build one for --kind with --json or --interactive"
        ),
    };

    let envelope = Envelope {
        target: target(repo, &target_arg)?,
        payload: payload.into(),
        payload_kind: kind.as_str().to_owned(),
        key: key_claim.into(),
    };
    println!("{}", claims.sign(&envelope)?);
    Ok(())
}

/// `revoke`: append a revocation of `<claim-id>` to that claim's own chain.
fn cmd_revoke(
    repo: &gix::Repository,
    store: &RepoStore<'_>,
    key: &SigningKey,
    args: RevokeArgs,
) -> Result<()> {
    ensure_schemas(store)?;
    let claims = Claims::open(store);
    let key_claim = key_claim(repo, store, key, args.key.as_deref(), args.machine)?;
    let claim = object(repo, &args.claim)?;
    println!("{}", claims.revoke(claim, key_claim.into())?);
    Ok(())
}

/// `verify`: the cryptographic verdict on one claim, worded and exit-coded so
/// it cannot be read as a statement about the claim's validity.
///
/// Exit codes: 0 verified, 1 bad signature, 2 nothing checked. A caller
/// branching on 0 has learned that the bytes are the key's, and nothing else —
/// revocation is `resolve`'s answer, and admissibility is a query rule's.
fn cmd_verify(repo: &gix::Repository, store: &RepoStore<'_>, args: VerifyArgs) -> Result<ExitCode> {
    let claims = Claims::open(store);
    let id = object(repo, &args.claim)?;
    let claim = Claim {
        id,
        envelope: claims
            .envelope_at(id)
            .with_context(|| format!("{id} is not a claim"))?,
        revoked_by: None,
    };
    let verdict = claims.verify(&claim)?;

    let (word, note, code) = match verdict {
        Verdict::Verified => (
            "Verified",
            "the signature is the signing key's over these commit bytes. \
             This is cryptography only: it does not say the claim is valid, \
             unrevoked, or admissible — see `git attest resolve` for revocation \
             and a query rule for validity.",
            0,
        ),
        Verdict::BadSignature => (
            "BadSignature",
            "the signature is not the signing key's over these commit bytes — \
             a forgery, a tampered commit, an unparsable signature, or no \
             signature at all. Cryptography distinguishes none of those.",
            1,
        ),
        Verdict::UnknownKeyFormat => (
            "UnknownKeyFormat",
            "no shipped verifier claims the key's format, or the stored \
             block's armor disagrees with it. Nothing was checked, and nothing \
             is asserted.",
            2,
        ),
    };

    if args.json {
        let mut object = VObject::new();
        object.insert("claim", id.to_string());
        object.insert("verdict", word);
        object.insert("cryptographic_only", true);
        println!("{}", facet_json::to_string(&Value::from(object))?);
    } else {
        println!("{word}: {note}");
    }
    Ok(ExitCode::from(code))
}

/// The `Target` a `<kind>:<hex>` argument names.
///
/// The kind is whatever was written — no allow-list, no vocabulary, no check
/// beyond it being a non-empty label.
fn target(repo: &gix::Repository, arg: &TargetArg) -> Result<Target> {
    Ok(Target {
        kind: arg.kind.clone(),
        id: object(repo, &arg.id)?.into(),
    })
}

/// The object `hex` names: a full hash as written, or anything `git rev-parse`
/// would resolve — an abbreviated prefix, most usefully.
fn object(repo: &gix::Repository, hex: &str) -> Result<ObjectId> {
    if let Ok(id) = ObjectId::from_hex(hex.as_bytes()) {
        return Ok(id);
    }
    Ok(repo
        .rev_parse_single(hex)
        .with_context(|| format!("not an object id: {hex:?}"))?
        .detach())
}

/// The key claim to name in an envelope: the one given, or the claim
/// publishing this run's signing key — added when the store has never seen it.
///
/// Key lifecycle is not a sixth subcommand: a key add is what happens the first
/// time a key signs anything, and a rotation is `--key`'s business, since the
/// user who rotated knows which link they mean.
fn key_claim(
    repo: &gix::Repository,
    store: &RepoStore<'_>,
    key: &SigningKey,
    given: Option<&str>,
    machine: bool,
) -> Result<ObjectId> {
    let claims = Claims::open(store);
    if let Some(hex) = given {
        let claim = object(repo, hex)?;
        claims
            .key(claim)
            .with_context(|| format!("--key {hex}: not a key claim"))?;
        return Ok(claim);
    }

    let attest_key = AttestKey::from_openssh(key.public(), machine)?;
    // The key document's own tree hash is the key's identity, so the chain to
    // look on is derivable without having written anything — the same
    // derivation `add_key` performs, through the same kind handle.
    let payload = store
        .kind::<AttestKey>(segment(KEY_KIND))
        .compile(&attest_key)?;
    let target = Target {
        kind: KEY_TARGET_KIND.to_owned(),
        id: payload.into(),
    };
    // Newest first, so the oldest match is the key-add at the chain's root:
    // the link every later rotation is checked against.
    let existing = claims
        .log(&target)?
        .filter(|claim| ObjectId::from(claim.envelope.payload) == payload)
        .last();
    match existing {
        Some(claim) => Ok(claim.id),
        None => Ok(claims.add_key(&attest_key)?),
    }
}

/// Register attest's own schemas if this store has none yet, so a first `sign`
/// in a fresh repository works rather than reporting a missing schema.
fn ensure_schemas(store: &RepoStore<'_>) -> Result<()> {
    let kinds = store.kinds()?;
    let has = |kind: &str| kinds.iter().any(|name| name.as_str() == kind);
    if !has(gix_attest::CLAIM_KIND) || !has(KEY_KIND) {
        register_schemas(store)?;
    }
    Ok(())
}

/// Write `value` as an entity of `kind` and return its document tree — the
/// payload hash the envelope carries.
///
/// The entity's name is the document's own tree hash, so writing the identical
/// payload twice writes the identical entity, and the payload stays reachable:
/// an envelope carries a hash, not the bytes.
fn write_payload(store: &RepoStore<'_>, kind: &RefSegment, value: Value) -> Result<ObjectId> {
    let dynamic = store.dynamic(kind.clone());
    let tree = dynamic
        .compile(&value)
        .with_context(|| format!("compiling a {kind} document"))?;
    let name = RefSegment::new(tree.to_string())
        .expect("a hex tree hash is a valid ref segment")
        .into();
    dynamic.put(&name, &value)?;
    Ok(tree)
}

/// `raw` as a `facet_value::Value` object.
fn parse_json(raw: &str) -> Result<Value> {
    let value: Value = facet_json::from_str(raw).context("parsing --json value")?;
    if value.as_object().is_none() {
        bail!("--json value must be a JSON object");
    }
    Ok(value)
}

/// Build a document of `kind` by prompting for the fields its published schema
/// names, one answer per line on stdin.
///
/// The shape, the defaults, and the refusal to build an incomplete document are
/// all [`DocumentBuilder`]'s: this function contributes the prompts and nothing
/// else. Answers are JSON literals, except that a `String`-shaped field takes
/// its line verbatim (quoting a string to write a string would be a tax with no
/// payer), and an empty answer to a field the schema can default leaves it
/// unset.
fn prompt_document(store: &RepoStore<'_>, kind: &RefSegment) -> Result<Value> {
    let schema = store
        .dynamic(kind.clone())
        .schema()
        .get()?
        .ok_or_else(|| anyhow!("no schema published for kind {kind}"))?;
    let mut builder = DocumentBuilder::for_schema(&schema)?;
    let fields: Vec<(String, Node, bool)> = builder
        .fields()
        .map(|(name, node, has_default)| (name.to_owned(), node.clone(), has_default))
        .collect();

    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    for (name, node, has_default) in fields {
        let node = resolve(&schema, &node).unwrap_or(&Node::Dynamic);
        let string_like = matches!(node, Node::String | Node::Bytes);
        let optional = matches!(node, Node::Optional(_));
        // Prompts go to stderr, leaving stdout for the claim id.
        eprint!("{name}: ");
        let raw = match lines.next() {
            Some(line) => line.context("reading stdin")?,
            None if has_default || optional => break,
            None => bail!("unexpected end of input while filling {name}"),
        };
        let raw = raw.trim_end_matches(['\n', '\r']);
        if raw.trim().is_empty() {
            if optional {
                builder.set(&name, Value::NULL)?;
                continue;
            }
            if has_default {
                continue;
            }
        }
        let value = if string_like {
            Value::from(raw)
        } else {
            facet_json::from_str::<Value>(raw.trim())
                .with_context(|| format!("{name}: expected a JSON value, got {raw:?}"))?
        };
        builder.set(&name, value)?;
    }
    builder
        .build()
        .with_context(|| format!("kind {kind}: the document is incomplete"))
}

/// One [`Node::Ref`] indirection into `schema.defs`, or the node itself when it
/// is not a `Ref`.
fn resolve<'s>(schema: &'s Schema, node: &'s Node) -> Option<&'s Node> {
    match node {
        Node::Ref(name) => schema.defs.get(name),
        other => Some(other),
    }
}

/// A chain, newest first. `revoked_by` is only ever set by `resolve`, so the
/// mark appears exactly where the chain records one.
fn print_claims(claims: &[Claim], json: bool) -> Result<()> {
    for claim in claims {
        if json {
            let mut object = VObject::new();
            object.insert("claim", claim.id.to_string());
            object.insert("target_kind", claim.envelope.target.kind.as_str());
            object.insert(
                "target_id",
                ObjectId::from(claim.envelope.target.id).to_string(),
            );
            object.insert(
                "payload",
                ObjectId::from(claim.envelope.payload).to_string(),
            );
            object.insert("payload_kind", claim.envelope.payload_kind.as_str());
            object.insert("key", ObjectId::from(claim.envelope.key).to_string());
            object.insert(
                "revoked_by",
                match claim.revoked_by {
                    Some(id) => Value::from(id.to_string().as_str()),
                    None => Value::NULL,
                },
            );
            println!("{}", facet_json::to_string(&Value::from(object))?);
        } else {
            println!("claim {}", claim.id);
            println!(
                "target: {}:{}",
                claim.envelope.target.kind,
                ObjectId::from(claim.envelope.target.id)
            );
            println!(
                "payload: {} ({})",
                ObjectId::from(claim.envelope.payload),
                claim.envelope.payload_kind
            );
            println!("key: {}", ObjectId::from(claim.envelope.key));
            if let Some(revocation) = claim.revoked_by {
                println!("revoked-by: {revocation}");
            }
            println!();
        }
    }
    Ok(())
}
