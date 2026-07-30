//! Chain integrity: concurrent claims on one target serialize instead of
//! forking.
//!
//! Nothing here is attest's mechanism — the compare-and-swap is
//! `gix-store`'s, over a real repository's refs, and a lost race retries on
//! the new tip. The test exists because that property *is* the chain
//! guarantee: a claim ref that only ever advances by one commit whose parent
//! is the expected tip cannot fork silently.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use std::path::Path;
use std::process::Command;

use gix_attest::{Claims, Envelope, Target, layout, register_claim_schema, target_key};
use gix_store::{GixRefStore, Store};

const WRITERS: usize = 4;
const CLAIMS_EACH: usize = 4;

fn target() -> Target {
    Target {
        kind: "commit".to_owned(),
        id: gix::ObjectId::from_hex(b"7f3e000000000000000000000000000000000000")
            .unwrap()
            .into(),
    }
}

fn envelope(payload_kind: String) -> Envelope {
    Envelope {
        target: target(),
        payload: gix::ObjectId::empty_tree(gix::hash::Kind::Sha1).into(),
        payload_kind,
        key: gix::ObjectId::null(gix::hash::Kind::Sha1).into(),
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

#[test]
fn concurrent_claims_on_one_target_all_land_on_one_unforked_chain() {
    let dir = tempfile::tempdir().unwrap();
    test_support::init_repo(dir.path());
    {
        let repo = gix::open(dir.path()).unwrap();
        let store = Store::with_layout(GixRefStore::new(&repo), &repo.objects, layout());
        register_claim_schema(&store).unwrap();
    }

    // Each writer is its own repository handle, so the races are real ones
    // resolved through the filesystem, not an in-process fiction.
    std::thread::scope(|scope| {
        for writer in 0..WRITERS {
            let path = dir.path();
            scope.spawn(move || {
                let repo = gix::open(path).unwrap();
                let store = Store::with_layout(GixRefStore::new(&repo), &repo.objects, layout());
                let claims = Claims::open(&store);
                for n in 0..CLAIMS_EACH {
                    claims
                        .sign(&envelope(format!("writer-{writer}-{n}")))
                        .unwrap();
                }
            });
        }
    });

    let reference = format!("refs/claims/{}", target_key(&target()).unwrap());

    // Every claim landed: nothing was lost to a race.
    let total = WRITERS * CLAIMS_EACH;
    let history = git(dir.path(), &["log", "--format=%H %P", &reference]);
    assert_eq!(history.lines().count(), total, "{history}");

    // And the chain is linear: every claim but the root has exactly one
    // parent, which is the next line's claim — the ref advanced one commit at
    // a time, over the tip each write was read against.
    let ids: Vec<&str> = history
        .lines()
        .map(|line| line.split(' ').next().unwrap())
        .collect();
    for (claim, line) in ids.iter().zip(history.lines()) {
        let parents: Vec<&str> = line.split(' ').skip(1).collect();
        assert!(parents.len() <= 1, "{claim} forked the chain: {line}");
    }
    let parents: Vec<&str> = history
        .lines()
        .filter_map(|line| line.split(' ').nth(1))
        .filter(|parent| !parent.is_empty())
        .collect();
    assert_eq!(parents, ids[1..], "each claim's parent is the previous tip");

    // Every payload kind occurs once, so the retries re-committed rather than
    // dropping or duplicating a claim.
    let mut kinds: Vec<String> = ids
        .iter()
        .map(|id| {
            git(
                dir.path(),
                &["cat-file", "-p", &format!("{id}:value/payload_kind")],
            )
            .trim()
            .to_owned()
        })
        .collect();
    kinds.sort();
    kinds.dedup();
    assert_eq!(kinds.len(), total);

    // Read back through the library, the same chain comes out newest-first.
    let repo = gix::open(dir.path()).unwrap();
    let store = Store::with_layout(GixRefStore::new(&repo), &repo.objects, layout());
    let log: Vec<String> = Claims::open(&store)
        .log(&target())
        .unwrap()
        .map(|claim| claim.id.to_string())
        .collect();
    assert_eq!(log, ids, "`log` walks the chain git walks, in git's order");
}
