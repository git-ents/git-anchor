//! Drive the built `git-attest` binary against a temp repo, exactly as
//! `git attest …` would.
//!
//! Signatures here are real: the CLI shells out to `ssh-keygen -Y sign` with a
//! key this test generates, so the claims written are ordinary ssh-signed
//! commits. That is what makes `git verify-commit` usable as an oracle — the
//! cross-check that proves the CLI's signing path against the real tool rather
//! than against our own reader.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use facet::Facet;
use gix_attest::{Target, target_key};
use gix_store::{Layout, RefPrefix, RefSegment, RepoStore};
use test_support::init_repo;

const BIN: &str = env!("CARGO_BIN_EXE_git-attest");

/// A payload kind attest knows nothing about: a review assertion, whose
/// vocabulary owner would be forge. The CLI writes one through store's dynamic
/// write path when `--json`/`--interactive` builds it.
#[derive(Facet)]
struct Review {
    verdict: String,
    rounds: u64,
}

/// Run the binary in `dir`, returning `(stdout, stderr, exit code)`.
fn run(dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("the binary exited normally"),
    )
}

/// Run the binary with `stdin` piped in — `sign --interactive`'s one answer per
/// line.
fn run_with_stdin(dir: &Path, args: &[&str], stdin: &str) -> (String, String, i32) {
    let mut child = Command::new(BIN)
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("the binary exited normally"),
    )
}

/// `git -C dir …`, as `(stdout, stderr, ok)`.
fn git(dir: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// An ed25519 key at `<dir>/<name>`, and its public line.
fn ssh_key(dir: &Path, name: &str) -> (PathBuf, String) {
    let key = dir.join(name);
    let status = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", name, "-f"])
        .arg(&key)
        .status()
        .expect("run ssh-keygen");
    assert!(status.success(), "ssh-keygen keygen failed");
    let public = std::fs::read_to_string(dir.join(format!("{name}.pub"))).expect("read public key");
    (key, public)
}

/// A repo with one commit, an ssh signing key configured the way git itself
/// configures one, and a `review` payload schema published by its own
/// vocabulary owner — which is not attest.
fn setup(dir: &Path) -> (PathBuf, String) {
    init_repo(dir);
    std::fs::write(dir.join("file.txt"), "one\n").unwrap();
    let (_, err, ok) = git(dir, &["add", "-A"]);
    assert!(ok, "{err}");
    let (_, err, ok) = git(dir, &["commit", "-q", "-m", "one"]);
    assert!(ok, "{err}");

    let (key, public) = ssh_key(dir, "signing");
    let allowed = dir.join("allowed_signers");
    std::fs::write(
        &allowed,
        format!("test@example.com namespaces=\"git\" {public}"),
    )
    .unwrap();
    let mut config = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.join(".git/config"))
        .unwrap();
    writeln!(
        config,
        "[gpg]\n\tformat = ssh\n[gpg \"ssh\"]\n\tallowedSignersFile = {}\n[user]\n\tsigningkey = {}",
        allowed.display(),
        key.display()
    )
    .unwrap();

    let repo = gix::open(dir).unwrap();
    let store = RepoStore::open_with_layout(&repo, layout());
    store
        .kind::<Review>(RefSegment::new("review").unwrap())
        .publish()
        .unwrap();

    let head = git(dir, &["rev-parse", "HEAD"]).0.trim().to_owned();
    (key, head)
}

/// The `{data, schema}` layout `git-attest` derives from its default
/// `--prefix`, duplicated here the way a fixture publisher would have it.
fn layout() -> Layout {
    let prefix = RefPrefix::new("refs").unwrap();
    Layout {
        data: prefix.clone(),
        schema: prefix.child(&RefSegment::new("schema").unwrap()),
    }
}

/// The `field: value` lines of the claim block naming `id` in `log`/`resolve`
/// output.
fn block(output: &str, id: &str) -> Vec<String> {
    output
        .split("\n\n")
        .find(|block| block.starts_with(&format!("claim {id}")))
        .unwrap_or_else(|| panic!("no block for claim {id} in:\n{output}"))
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The `claim <id>` lines of `log`/`resolve` output, newest first.
fn ids(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| line.strip_prefix("claim "))
        .map(str::to_owned)
        .collect()
}

// ── sign, log, resolve ──────────────────────────────────────────────────

/// The whole chain, end to end: two claims about one target, both on that
/// target's ref, newest first, each carrying the payload and kind it was signed
/// with — and the same key claim, because a key add happens once.
#[test]
fn sign_chains_claims_on_one_target_and_log_walks_them_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let (key, head) = setup(dir.path());
    let key = key.to_str().unwrap();
    let empty_tree = gix::ObjectId::empty_tree(gix::hash::Kind::Sha1).to_string();

    let (first, err, code) = run(
        dir.path(),
        &[
            "sign",
            &format!("anchor:{head}"),
            &empty_tree,
            "--kind",
            "review",
            "--signing-key",
            key,
        ],
    );
    assert_eq!(code, 0, "sign failed: {err}");
    let first = first.trim().to_owned();

    let (second, err, code) = run(
        dir.path(),
        &[
            "sign",
            &format!("anchor:{head}"),
            &empty_tree,
            "--kind",
            "review",
            "--signing-key",
            key,
        ],
    );
    assert_eq!(code, 0, "sign failed: {err}");
    let second = second.trim().to_owned();
    assert_ne!(first, second, "two claims are two commits");

    let (out, err, code) = run(dir.path(), &["log", &format!("anchor:{head}")]);
    assert_eq!(code, 0, "log failed: {err}");
    assert_eq!(ids(&out), vec![second.clone(), first.clone()]);

    let lines = block(&out, &first);
    assert!(
        lines.contains(&format!("target: anchor:{head}")),
        "{lines:?}"
    );
    assert!(
        lines.contains(&format!("payload: {empty_tree} (review)")),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.starts_with("revoked-by:")),
        "an unrevoked claim is unmarked: {lines:?}"
    );

    // One key add for one key: the second claim names the same key claim.
    let key_line = |id: &str| {
        block(&out, id)
            .into_iter()
            .find(|line| line.starts_with("key: "))
            .unwrap()
    };
    assert_eq!(key_line(&first), key_line(&second));

    // The claims are on `refs/claims/<target-key>`, the specified layout, and
    // the tip is the newest claim.
    let target = Target {
        kind: "anchor".to_owned(),
        id: gix::ObjectId::from_hex(head.as_bytes()).unwrap().into(),
    };
    let reference = format!("refs/claims/{}", target_key(&target).unwrap());
    assert_eq!(git(dir.path(), &["rev-parse", &reference]).0.trim(), second);
}

/// `<kind>:<hex>` and nothing else, with the hex half resolved the way git
/// resolves any object name — and no allow-list of kinds: a label nobody has
/// ever published works exactly as `anchor` does.
#[test]
fn a_target_is_kind_colon_hex_with_no_vocabulary_of_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let (key, head) = setup(dir.path());
    let key = key.to_str().unwrap();
    let empty_tree = gix::ObjectId::empty_tree(gix::hash::Kind::Sha1).to_string();
    let sign = |target: &str| {
        run(
            dir.path(),
            &[
                "sign",
                target,
                &empty_tree,
                "--kind",
                "review",
                "--signing-key",
                key,
            ],
        )
    };

    let (_, err, code) = sign("hyperbolic-widget:0000000");
    assert_ne!(code, 0, "an unresolvable hash is refused");
    assert!(err.contains("not an object id"), "{err}");

    let (_, err, code) = sign(&head);
    assert_ne!(code, 0, "a bare hash is not a target");
    assert!(err.contains("<kind>:<hex>"), "{err}");

    // An abbreviated hash resolves, and an invented kind is just a label.
    let (id, err, code) = sign(&format!("hyperbolic-widget:{}", &head[..7]));
    assert_eq!(code, 0, "sign failed: {err}");
    let (out, _err, code) = run(dir.path(), &["log", &format!("hyperbolic-widget:{head}")]);
    assert_eq!(code, 0);
    assert_eq!(ids(&out), vec![id.trim().to_owned()]);
    assert!(
        block(&out, id.trim()).contains(&format!("target: hyperbolic-widget:{head}")),
        "{out}"
    );
}

/// A payload built from `--json` and one built interactively both go through
/// store's dynamic write path, land as entities of their own kind, and are
/// carried as a tree hash the claim never interprets.
#[test]
fn a_payload_document_can_be_built_instead_of_named() {
    let dir = tempfile::tempdir().unwrap();
    let (key, head) = setup(dir.path());
    let key = key.to_str().unwrap();

    let (from_json, err, code) = run(
        dir.path(),
        &[
            "sign",
            &format!("commit:{head}"),
            "--kind",
            "review",
            "--json",
            r#"{"verdict": "approved", "rounds": 2}"#,
            "--signing-key",
            key,
        ],
    );
    assert_eq!(code, 0, "sign --json failed: {err}");

    let (interactive, err, code) = run_with_stdin(
        dir.path(),
        &[
            "sign",
            &format!("commit:{head}"),
            "--kind",
            "review",
            "--interactive",
            "--signing-key",
            key,
        ],
        // One answer per field, in the schema's order: `rounds`, `verdict`.
        "2\napproved\n",
    );
    assert_eq!(code, 0, "sign --interactive failed: {err}");

    let (out, _err, code) = run(dir.path(), &["log", &format!("commit:{head}")]);
    assert_eq!(code, 0);
    let payload = |id: &str| {
        block(&out, id)
            .into_iter()
            .find(|line| line.starts_with("payload: "))
            .unwrap()
    };
    assert_eq!(
        payload(from_json.trim()),
        payload(interactive.trim()),
        "the same document, built two ways, is the same payload hash"
    );

    // The payload is a real entity of its kind, readable back through the
    // store — attest carried the hash and never looked.
    let hash = payload(from_json.trim())
        .trim_start_matches("payload: ")
        .trim_end_matches(" (review)")
        .to_owned();
    let repo = gix::open(dir.path()).unwrap();
    let store = RepoStore::open_with_layout(&repo, layout());
    let kind = store.kind::<Review>(RefSegment::new("review").unwrap());
    let review = kind
        .decode(gix::ObjectId::from_hex(hash.as_bytes()).unwrap())
        .unwrap();
    assert_eq!(review.verdict, "approved");
    assert_eq!(review.rounds, 2);
}

/// `revoke` appends a revocation to the revoked claim's own chain, and
/// `resolve` marks exactly that claim — the other claim on the same chain is
/// untouched, and `log` reports the chain as written, marking nothing.
#[test]
fn revoke_then_resolve_marks_exactly_that_claim() {
    let dir = tempfile::tempdir().unwrap();
    let (key, head) = setup(dir.path());
    let key = key.to_str().unwrap();
    let empty_tree = gix::ObjectId::empty_tree(gix::hash::Kind::Sha1).to_string();
    let target = format!("anchor:{head}");

    let sign = || {
        let (out, err, code) = run(
            dir.path(),
            &[
                "sign",
                &target,
                &empty_tree,
                "--kind",
                "review",
                "--signing-key",
                key,
            ],
        );
        assert_eq!(code, 0, "sign failed: {err}");
        out.trim().to_owned()
    };
    let doomed = sign();
    let spared = sign();

    let (revocation, err, code) = run(dir.path(), &["revoke", &doomed, "--signing-key", key]);
    assert_eq!(code, 0, "revoke failed: {err}");
    let revocation = revocation.trim().to_owned();

    let (out, err, code) = run(dir.path(), &["resolve", &target]);
    assert_eq!(code, 0, "resolve failed: {err}");
    assert_eq!(
        ids(&out),
        vec![revocation.clone(), spared.clone(), doomed.clone()],
        "the revocation chains on the revoked claim's own ref"
    );
    assert!(
        block(&out, &doomed).contains(&format!("revoked-by: {revocation}")),
        "{out}"
    );
    for other in [&spared, &revocation] {
        assert!(
            !block(&out, other)
                .iter()
                .any(|line| line.starts_with("revoked-by:")),
            "exactly one claim is marked: {out}"
        );
    }
    assert!(
        block(&out, &revocation).contains(&format!("target: claim:{doomed}")),
        "a revocation is a claim about a claim: {out}"
    );

    // `log` has no opinion: the same chain, nothing marked.
    let (out, _err, code) = run(dir.path(), &["log", &target]);
    assert_eq!(code, 0);
    assert_eq!(ids(&out), vec![revocation, spared, doomed]);
    assert!(!out.contains("revoked-by"), "log marks nothing: {out}");
}

// ── verify: cryptography, and the real tool as the oracle ───────────────

/// `verify` reports a sound signature, says in the same breath that soundness
/// is not validity, and — the cross-check that matters — the very claim it
/// accepted is accepted by stock `git verify-commit`, which knows nothing about
/// attest.
#[test]
fn verify_reports_verified_and_stock_git_agrees() {
    let dir = tempfile::tempdir().unwrap();
    let (key, head) = setup(dir.path());
    let key = key.to_str().unwrap();
    let empty_tree = gix::ObjectId::empty_tree(gix::hash::Kind::Sha1).to_string();

    let (id, err, code) = run(
        dir.path(),
        &[
            "sign",
            &format!("anchor:{head}"),
            &empty_tree,
            "--kind",
            "review",
            "--signing-key",
            key,
        ],
    );
    assert_eq!(code, 0, "sign failed: {err}");
    let id = id.trim().to_owned();

    let (out, err, code) = run(dir.path(), &["verify", &id]);
    assert_eq!(code, 0, "verify failed: {err}");
    assert!(out.starts_with("Verified: "), "{out}");
    assert!(
        out.contains("cryptography only") && out.contains("does not say the claim is valid"),
        "the report must not let soundness be read as validity: {out}"
    );

    let (out, _err, code) = run(dir.path(), &["verify", &id, "--json"]);
    assert_eq!(code, 0);
    assert!(out.contains("\"verdict\""), "{out}");
    assert!(out.contains("Verified"), "{out}");
    assert!(out.contains("cryptographic_only"), "{out}");

    // The oracle: git's own verifier, on git's own `gpgsig` header, with none
    // of our tooling involved.
    let (_, report, ok) = git(dir.path(), &["verify-commit", "-v", &id]);
    assert!(
        ok,
        "git verify-commit rejected a claim this CLI signed: {report}"
    );
    assert!(
        report.contains("Good \"git\" signature"),
        "expected a good-signature report, got {report:?}"
    );
}

/// A tampered claim commit: the same signature over changed bytes. `verify`
/// says `BadSignature` and exits non-zero, and `git verify-commit` agrees.
#[test]
fn verify_fails_on_a_tampered_claim() {
    let dir = tempfile::tempdir().unwrap();
    let (key, head) = setup(dir.path());
    let key = key.to_str().unwrap();
    let empty_tree = gix::ObjectId::empty_tree(gix::hash::Kind::Sha1).to_string();

    let (id, err, code) = run(
        dir.path(),
        &[
            "sign",
            &format!("anchor:{head}"),
            &empty_tree,
            "--kind",
            "review",
            "--signing-key",
            key,
        ],
    );
    assert_eq!(code, 0, "sign failed: {err}");
    let id = id.trim().to_owned();

    // Rewrite the claim commit with one byte of its message changed, through
    // git itself, so the forgery is real objects in the repository.
    let raw = git(dir.path(), &["cat-file", "commit", &id]).0;
    let flipped = raw.replace("claim review", "claim REVIEW");
    assert_ne!(raw, flipped, "the message was there to tamper with");
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir.path())
        .args(["hash-object", "-t", "commit", "-w", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(flipped.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let tampered = String::from_utf8_lossy(&out.stdout).trim().to_owned();

    let (out, err, code) = run(dir.path(), &["verify", &tampered]);
    assert_eq!(code, 1, "a bad signature exits 1: {err}");
    assert!(out.starts_with("BadSignature: "), "{out}");

    let (_, _report, ok) = git(dir.path(), &["verify-commit", &tampered]);
    assert!(!ok, "git verify-commit accepted a tampered claim");
}

/// `verify` needs a claim, and says so: a commit that is not one is an error,
/// not a verdict.
#[test]
fn verify_refuses_a_commit_that_is_not_a_claim() {
    let dir = tempfile::tempdir().unwrap();
    let (_key, head) = setup(dir.path());
    let (_out, err, code) = run(dir.path(), &["verify", &head]);
    assert_ne!(code, 0);
    assert!(err.contains("not a claim"), "{err}");
}
