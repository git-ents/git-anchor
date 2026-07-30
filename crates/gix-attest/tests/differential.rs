//! The differential fixture: a claim written by `gix-attest` is an ordinary
//! git commit, readable by stock `git cat-file` with no tooling of ours
//! installed — plain commits, plain trees, and no claim data hidden in
//! trailers.
//!
//! Real `git` is the oracle throughout: every assertion below is made by
//! shelling out to the installed binary, not by reading the objects back
//! through the same library that wrote them.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use std::path::Path;
use std::process::Command;

use gix_attest::{Claims, Envelope, Target, layout, register_claim_schema, target_key};
use gix_refstore::{SignatureBytes, Signer};
use gix_store::{GixRefStore, Store};

/// A signer standing in for `ssh-keygen -Y sign`: the store carries whatever
/// bytes it is handed, and this phase has no crypto to hand it. The *shape*
/// is what is under test here — an armored block in the `gpgsig` header —
/// and Phase 2 replaces the bytes with real ones.
struct FakeArmor;

impl Signer for FakeArmor {
    type Error = std::io::Error;

    fn sign(&self, _bytes: &[u8]) -> Result<SignatureBytes, Self::Error> {
        Ok(SignatureBytes::from(
            b"-----BEGIN SSH SIGNATURE-----\nZmFrZQ==\n-----END SSH SIGNATURE-----".to_vec(),
        ))
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git output is utf-8")
}

fn envelope(kind: &str, payload_kind: &str) -> Envelope {
    Envelope {
        target: Target {
            kind: kind.to_owned(),
            id: gix::ObjectId::from_hex(b"7f3e000000000000000000000000000000000000")
                .unwrap()
                .into(),
        },
        payload: gix::ObjectId::empty_tree(gix::hash::Kind::Sha1).into(),
        payload_kind: payload_kind.to_owned(),
        key: gix::ObjectId::null(gix::hash::Kind::Sha1).into(),
    }
}

#[test]
fn a_claim_is_a_plain_commit_stock_git_reads() {
    let dir = tempfile::tempdir().unwrap();
    test_support::init_repo(dir.path());
    let repo = gix::open(dir.path()).unwrap();
    let store = Store::with_layout(GixRefStore::new(&repo), &repo.objects, layout());
    register_claim_schema(&store).unwrap();

    let claims = Claims::open(&store);
    // `"anchor"` here is a label this crate never interprets: no anchor
    // vocabulary is involved in writing or reading this claim.
    let pin = envelope("anchor", "rebind-pin");
    let first = claims.sign(&pin).unwrap();
    let second = claims.sign(&envelope("anchor", "review")).unwrap();

    let key = target_key(&pin.target).unwrap();
    let reference = format!("refs/claims/{key}");

    // The ref is where the layout says, and its tip is the newest claim id.
    assert_eq!(
        git(dir.path(), &["rev-parse", &reference]).trim(),
        second.to_string()
    );

    // The chain is a first-parent commit chain git walks itself, newest
    // first — the claim ids are the commit oids, with no side mapping.
    assert_eq!(
        git(dir.path(), &["log", "--format=%H", &reference])
            .lines()
            .collect::<Vec<_>>(),
        vec![second.to_string(), first.to_string()]
    );
    assert_eq!(
        git(dir.path(), &["cat-file", "-t", &second.to_string()]).trim(),
        "commit"
    );
    assert_eq!(
        git(dir.path(), &["rev-parse", &format!("{second}^")]).trim(),
        first.to_string(),
        "a claim's parent is the tip it was written over"
    );

    // The tree is store's ordinary `{value/, schema/}` document tree, walkable
    // with plumbing: nothing about a claim tree is special.
    let tree = git(
        dir.path(),
        &["cat-file", "-p", &format!("{second}^{{tree}}")],
    );
    let names: Vec<_> = tree
        .lines()
        .map(|line| line.rsplit('\t').next().unwrap())
        .collect();
    assert_eq!(names, vec!["schema", "value"]);

    // Every envelope field is a blob `git cat-file` prints as text.
    assert_eq!(
        git(
            dir.path(),
            &["cat-file", "-p", &format!("{second}:value/payload_kind")]
        )
        .trim(),
        "review"
    );
    assert_eq!(
        git(
            dir.path(),
            &["cat-file", "-p", &format!("{second}:value/target/kind")]
        )
        .trim(),
        "anchor"
    );

    // No claim data rides a trailer. Store's own `Schema:` provenance label
    // is the only trailer present, and it names a schema commit rather than
    // carrying any part of the claim.
    let message = git(dir.path(), &["log", "-1", "--format=%B", &reference]);
    let trailers: Vec<_> = message
        .lines()
        .filter(|line| line.contains(": ") && !line.starts_with(' '))
        .collect();
    assert_eq!(trailers.len(), 1, "{message}");
    assert!(trailers[0].starts_with("Schema: "), "{message}");
    assert_eq!(
        git(dir.path(), &["log", "-1", "--format=%s", &reference]).trim(),
        "claim review"
    );
}

/// The signature transport is git's: the bytes the caller's [`Signer`]
/// produced land verbatim in the standard `gpgsig` header, which stock `git
/// cat-file` shows — and which the claim's own tree does not contain, since
/// signed content cannot hold its own signature.
///
/// That git *accepts* the block is Phase 2's assertion, with `ssh-keygen -Y
/// sign` producing it and `git verify-commit` as the oracle; this phase has no
/// crypto, so it asserts only the transport.
#[test]
fn a_signed_claim_carries_its_signature_in_the_gpgsig_header() {
    let dir = tempfile::tempdir().unwrap();
    test_support::init_repo(dir.path());
    let repo = gix::open(dir.path()).unwrap();
    let store =
        Store::with_layout(GixRefStore::new(&repo), &repo.objects, layout()).with_signer(FakeArmor);
    register_claim_schema(&store).unwrap();

    let claim = Claims::open(&store)
        .sign(&envelope("commit", "review"))
        .unwrap();

    let raw = git(dir.path(), &["cat-file", "commit", &claim.to_string()]);
    assert!(
        raw.contains("gpgsig -----BEGIN SSH SIGNATURE-----\n ZmFrZQ==\n"),
        "the header is git's, folded onto git's continuation lines: {raw}"
    );
    assert_eq!(
        raw.matches("SSH SIGNATURE").count(),
        2,
        "one armored block, opened and closed: {raw}"
    );
    assert!(
        git(dir.path(), &["cat-file", "-p", &format!("{claim}:value")])
            .lines()
            .all(|line| !line.contains("gpgsig")),
        "the signature is on the commit, not in the tree"
    );

    // Unsigned claims stay unsigned: the signer is a property of the store,
    // and attest contributes no signing code of its own either way.
    let plain = Store::with_layout(GixRefStore::new(&repo), &repo.objects, layout());
    let plain_claim = Claims::open(&plain)
        .sign(&envelope("commit", "unsigned"))
        .unwrap();
    assert!(
        !git(
            dir.path(),
            &["cat-file", "commit", &plain_claim.to_string()]
        )
        .contains("gpgsig"),
        "a store with no signer writes a plain commit"
    );
}
