//! Verification against non-Rust oracles.
//!
//! Every signature here is produced by `ssh-keygen -Y sign` and checked three
//! ways: by this crate, by `ssh-keygen -Y verify`, and by `git verify-commit`.
//! The point of the `gpgsig` transport is that all three see the same bytes, so
//! a disagreement is a defect in the one that disagrees — and a claim this
//! crate calls [`Verdict::Verified`] that real git rejects would be the worst
//! kind of pass.
//!
//! Keys are generated with `ssh-keygen` into a tempdir; nothing is checked in.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use gix_attest::verify::signed_payload;
use gix_attest::{AttestKey, Claim, Claims, Envelope, Target, Verdict, layout, register_schemas};
use gix_refstore::{SignatureBytes, Signer};
use gix_store::{GixRefStore, Store};

/// Signs by shelling out to `ssh-keygen -Y sign`, so the bytes on the commit
/// are byte-for-byte what git's own ssh signing backend would have produced —
/// the same signer `../git-store`'s repository test uses, for the same reason.
struct SshKeygen {
    key: PathBuf,
}

impl Signer for SshKeygen {
    type Error = std::io::Error;

    fn sign(&self, bytes: &[u8]) -> Result<SignatureBytes, Self::Error> {
        // `-n git` is git's own SSHSIG namespace, and reading the payload from
        // stdin keeps the signed bytes off disk.
        let mut child = Command::new("ssh-keygen")
            .args(["-Y", "sign", "-q", "-n", "git", "-f"])
            .arg(&self.key)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        child.stdin.take().expect("piped stdin").write_all(bytes)?;
        let out = child.wait_with_output()?;
        if !out.status.success() {
            return Err(std::io::Error::other("ssh-keygen -Y sign failed"));
        }
        Ok(SignatureBytes::from(out.stdout))
    }
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

/// An allowed-signers file trusting `public` for `test@example.com` — the
/// identity `init_repo` configures, and the principal git hands `ssh-keygen -Y
/// verify` — plus the git config that points both at it.
fn trust(dir: &Path, public: &str) -> PathBuf {
    let allowed = dir.join("allowed_signers");
    std::fs::write(
        &allowed,
        format!("test@example.com namespaces=\"git\" {public}"),
    )
    .expect("write allowed signers");
    let mut config = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.join(".git/config"))
        .expect("open config");
    writeln!(
        config,
        "[gpg]\n\tformat = ssh\n[gpg \"ssh\"]\n\tallowedSignersFile = {}",
        allowed.display()
    )
    .expect("write config");
    allowed
}

fn envelope(payload_kind: &str, key: gix::ObjectId) -> Envelope {
    Envelope {
        target: Target {
            kind: "commit".to_owned(),
            id: gix::ObjectId::from_hex(b"7f3e000000000000000000000000000000000000")
                .unwrap()
                .into(),
        },
        payload: gix::ObjectId::empty_tree(gix::hash::Kind::Sha1).into(),
        payload_kind: payload_kind.to_owned(),
        key: key.into(),
    }
}

/// `git verify-commit`, as a yes-or-no plus its report.
fn git_verify_commit(dir: &Path, commit: gix::ObjectId) -> (bool, String) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["verify-commit", "-v"])
        .arg(commit.to_string())
        .output()
        .expect("run git verify-commit");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `ssh-keygen -Y verify` over the bytes this crate says the signature covers:
/// the oracle for the payload derivation as much as for the cryptography.
fn ssh_keygen_verify(dir: &Path, allowed: &Path, signature: &[u8], signed: &[u8]) -> bool {
    let sig = dir.join("payload.sig");
    std::fs::write(&sig, signature).expect("write signature");
    let mut child = Command::new("ssh-keygen")
        .args(["-Y", "verify", "-n", "git", "-I", "test@example.com", "-f"])
        .arg(allowed)
        .arg("-s")
        .arg(&sig)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("run ssh-keygen -Y verify");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(signed)
        .expect("write payload");
    child.wait().expect("ssh-keygen -Y verify").success()
}

/// The whole point, in one test: a claim signed here verifies here, under
/// `ssh-keygen -Y verify`, and under `git verify-commit` — and a bit-flipped
/// commit fails all three.
#[test]
fn a_signed_claim_verifies_here_and_under_both_real_tools() {
    let dir = tempfile::tempdir().unwrap();
    test_support::init_repo(dir.path());
    let (key, public) = ssh_key(dir.path(), "signing");
    let allowed = trust(dir.path(), &public);

    let repo = gix::open(dir.path()).unwrap();
    let store = Store::with_layout(GixRefStore::new(&repo), &repo.objects, layout())
        .with_signer(SshKeygen { key });
    register_schemas(&store).unwrap();
    let claims = Claims::open(&store);

    let key_claim = claims
        .add_key(&AttestKey::from_openssh(&public, false).unwrap())
        .unwrap();
    let id = claims.sign(&envelope("review", key_claim)).unwrap();
    let claim = claims
        .resolve(&envelope("review", key_claim).target)
        .unwrap()
        .into_iter()
        .find(|claim| claim.id == id)
        .expect("the claim is on its chain");

    // This crate.
    assert_eq!(claims.verify(&claim).unwrap(), Verdict::Verified);
    assert_eq!(
        claims.key(key_claim).unwrap().unwrap().format,
        "ssh-ed25519",
        "the key resolved through its own chain"
    );

    // `git verify-commit`: the transport, the payload, and the signature are
    // all git's, with no tooling of ours installed.
    let (ok, report) = git_verify_commit(dir.path(), id);
    assert!(ok, "git verify-commit rejected a claim: {report}");
    assert!(
        report.contains("Good \"git\" signature"),
        "expected a good-signature report, got {report:?}"
    );

    // `ssh-keygen -Y verify`, over the bytes `signed_payload` derives.
    let signature = store.signature(id).unwrap().expect("a signature");
    let signed = signed_payload(&store, id).unwrap();
    assert!(
        ssh_keygen_verify(dir.path(), &allowed, signature.as_bytes(), &signed),
        "ssh-keygen -Y verify rejected the payload this crate derived"
    );
    assert!(
        !ssh_keygen_verify(dir.path(), &allowed, signature.as_bytes(), b"other bytes"),
        "the oracle is not vacuous: other bytes do not verify"
    );

    // A bit-flipped commit: the same signature over changed bytes. The commit
    // is re-written through `git hash-object`, so the tampering is real objects
    // in the repository, and every checker sees the same forgery.
    let raw = Command::new("git")
        .arg("-C")
        .arg(dir.path())
        .args(["cat-file", "commit"])
        .arg(id.to_string())
        .output()
        .expect("run git cat-file");
    let flipped = String::from_utf8(raw.stdout)
        .unwrap()
        .replace("claim review", "claim REVIEW");
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir.path())
        .args(["hash-object", "-t", "commit", "-w", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("run git hash-object");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(flipped.as_bytes())
        .expect("write commit");
    let out = child.wait_with_output().expect("git hash-object");
    let tampered =
        gix::ObjectId::from_hex(String::from_utf8(out.stdout).unwrap().trim().as_bytes()).unwrap();

    let forged = Claim {
        id: tampered,
        envelope: claim.envelope.clone(),
        revoked_by: None,
    };
    assert_eq!(
        claims.verify(&forged).unwrap(),
        Verdict::BadSignature,
        "a bit-flipped commit is not verified"
    );
    let (ok, _report) = git_verify_commit(dir.path(), tampered);
    assert!(!ok, "git verify-commit accepted a bit-flipped commit");
    let tampered_signature = store.signature(tampered).unwrap().expect("a signature");
    assert!(
        !ssh_keygen_verify(
            dir.path(),
            &allowed,
            tampered_signature.as_bytes(),
            &signed_payload(&store, tampered).unwrap()
        ),
        "ssh-keygen -Y verify accepted a bit-flipped commit"
    );
}

/// A rotation is trusted through the key it replaces, not on its own word: a
/// claim signed by a rotated-to key verifies only when the rotation itself was
/// signed by the key it rotated away from.
#[test]
fn a_rotation_verifies_through_the_key_it_replaces() {
    let dir = tempfile::tempdir().unwrap();
    test_support::init_repo(dir.path());
    let (first_key, first_public) = ssh_key(dir.path(), "first");
    let (second_key, second_public) = ssh_key(dir.path(), "second");
    let (stranger_key, _stranger_public) = ssh_key(dir.path(), "stranger");

    let repo = gix::open(dir.path()).unwrap();
    let signing = |key: PathBuf| {
        Store::with_layout(GixRefStore::new(&repo), &repo.objects, layout())
            .with_signer(SshKeygen { key })
    };

    let as_first = signing(first_key);
    register_schemas(&as_first).unwrap();
    let added = Claims::open(&as_first)
        .add_key(&AttestKey::from_openssh(&first_public, false).unwrap())
        .unwrap();

    // The rotation is signed by the first key: the chain holds.
    let rotated = Claims::open(&as_first)
        .rotate_key(
            added,
            &AttestKey::from_openssh(&second_public, true).unwrap(),
        )
        .unwrap();
    let as_second = signing(second_key);
    let claims = Claims::open(&as_second);
    let id = claims.sign(&envelope("review", rotated)).unwrap();
    let claim = claims
        .log(&envelope("review", rotated).target)
        .unwrap()
        .find(|claim| claim.id == id)
        .expect("the claim is on its chain");
    assert_eq!(claims.verify(&claim).unwrap(), Verdict::Verified);
    assert!(
        claims.key(rotated).unwrap().unwrap().machine,
        "the rotated-to key is the one that resolved"
    );

    // A rotation signed by a stranger is not a rotation of this key, and the
    // claims under it are not verified — the failure is reported on the claim,
    // because a claim signed by an unreachable key is not verified.
    let as_stranger = signing(stranger_key);
    let usurped = Claims::open(&as_stranger)
        .rotate_key(
            rotated,
            &AttestKey::from_openssh(&first_public, false).unwrap(),
        )
        .unwrap();
    let usurped_claim = claims
        .sign(&envelope("usurped", usurped))
        .and_then(|id| {
            Ok(claims
                .log(&envelope("usurped", usurped).target)?
                .find(|claim| claim.id == id)
                .expect("the claim is on its chain"))
        })
        .unwrap();
    assert_eq!(
        claims.verify(&usurped_claim).unwrap(),
        Verdict::BadSignature,
        "a rotation not signed by the key it replaces breaks the chain"
    );
    assert_eq!(claims.key(usurped).unwrap(), None);
}
