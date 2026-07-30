//! Round-trip fixture for [`Binding::deserialize`]/[`Binding::serialize_into`]
//! against the stored `Binding::Position` format: an externally-tagged
//! `facet-git-tree` enum tree — a single `"Position"` entry whose oid is the
//! anchor's own tree, split into sibling `identity`/`hints` subtrees.
//!
//! Every oid below is content-addressed from the bytes reconstructed in
//! this file, so a single corrupted byte in any hex constant — or in the
//! reconstructed content itself — fails an `assert_eq!` here rather than
//! silently drifting.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test"
)]

use facet_git_tree::ObjectStore;
use gix_anchor::{Binding, LineRange};
use gix_object::tree::{Entry, EntryKind, EntryMode};
use gix_object::{Kind, Tree, Write as _};

/// The tagged `Binding::Position` root tree's oid: a single `"Position"`
/// entry pointing at [`ANCHOR_ROOT`].
const ROOT: &str = "12f8f63eb70970bea182f03f23203138b624cfe8";

/// The anchor's own tree oid: two entries, `"hints"` and `"identity"`.
const ANCHOR_ROOT: &str = "1ff166d825ab77d7a26092a2ac47173ed596b269";

/// `identity` subtree oid: `genesis`, `path`, `lines`.
const IDENTITY_ROOT: &str = "ff6b9582f0c1a6c4fcdeb338040ab4b86e9fdc60";
/// `hints` subtree oid: `blob`, `content`, `context`.
const HINTS_ROOT: &str = "87e2a089671b5859ca37852ea6d64351e7fcc669";

/// `hints.blob` entry: [`gix_anchor::AnchorHints::blob`]'s 20 raw bytes,
/// embedded — equal to `CONTENT_OID`'s own bytes, by content addressing
/// (`anchor.retention`).
const BLOB_ENTRY_OID: &str = "50459e630a2f7b4507d79cf0de3cc5d85787d825";
const BLOB_ENTRY_RAW: &str = "fa2da6e55caa540725b55c04d13f1e42b4c725ce";

/// `identity.genesis` entry: [`gix_anchor::AnchorIdentity::genesis`]'s 20 raw
/// bytes, embedded (an arbitrary, best-effort commit id — it need not
/// resolve to a real object in this fixture).
const GENESIS_ENTRY_OID: &str = "13b6fa8bdbfa8bd548ddb6ef9f444bdb1033b220";
const GENESIS_ENTRY_RAW: &str = "92cf309c4efcf8698a5bd8f82d56f68fd38cc963";

/// `hints.content` entry: the anchored blob's bytes as serialized storage leaf.
const CONTENT_ENTRY_OID: &str = "3647a46fdccdf81dcf568159c84d88e660a95e3d";
/// Anchored blob oid (`hints.blob` entry payload).
const CONTENT_OID: &str = "fa2da6e55caa540725b55c04d13f1e42b4c725ce";
/// `hints.context` entry: a three-line margin around lines 3..=4, `"line 1\n"`
/// through `"line 7\n"`.
const CONTEXT_OID: &str = "167d19634ea4a2335202b28f0fd463da7d1df4b2";

const LINES_OID: &str = "5a429aeb1df6f3e6a457cd3a7fdb7b48bc685d1c";
const LINES_SOME_OID: &str = "87795e3d27f78f365a1561024559a36ae0409c76";
const LINES_END_OID: &str = "b8626c4cff2849624fb67f87cd0ad72b163671ad";
const LINES_START_OID: &str = "00750edc07d6415dcc07ae0351e9397b0222b7ba";

const PATH_OID: &str = "42d995590468e16e3a192a81518166b7dddac2a0";

fn oid(hex: &str) -> gix::ObjectId {
    gix::ObjectId::from_hex(hex.as_bytes()).expect("valid hex oid")
}

fn numbered(range: std::ops::RangeInclusive<u32>) -> String {
    range.map(|n| format!("line {n}\n")).collect()
}

fn write_blob(store: &ObjectStore, bytes: &[u8]) -> gix::ObjectId {
    store.write_buf(Kind::Blob, bytes).expect("write blob")
}

fn write_tree(store: &ObjectStore, mut entries: Vec<Entry>) -> gix::ObjectId {
    entries.sort();
    store.write(&Tree { entries }).expect("write tree")
}

fn entry(name: &str, kind: EntryKind, id: gix::ObjectId) -> Entry {
    Entry {
        mode: EntryMode::from(kind),
        filename: name.into(),
        oid: id,
    }
}

/// Reconstruct the fixture's object set with `gix_object::Tree` +
/// `gix_object::Write`, asserting every intermediate oid along the way
/// (item 1 of the fixture contract) before returning the finished store.
fn build_fixture() -> (gix::ObjectId, ObjectStore) {
    let store = ObjectStore::default();

    let mut blob_entry_bytes = oid(BLOB_ENTRY_RAW).as_slice().to_vec();
    blob_entry_bytes.push(b'\n');
    let blob_entry = write_blob(&store, &blob_entry_bytes);
    assert_eq!(blob_entry.to_string(), BLOB_ENTRY_OID);

    let mut genesis_entry_bytes = oid(GENESIS_ENTRY_RAW).as_slice().to_vec();
    genesis_entry_bytes.push(b'\n');
    let genesis_entry = write_blob(&store, &genesis_entry_bytes);
    assert_eq!(genesis_entry.to_string(), GENESIS_ENTRY_OID);

    let mut content_bytes = numbered(1..=10).into_bytes();
    content_bytes.push(b'\n');
    let content = write_blob(&store, &content_bytes);
    assert_eq!(content.to_string(), CONTENT_ENTRY_OID);

    let mut context_bytes = numbered(1..=7).into_bytes();
    context_bytes.push(b'\n');
    let context = write_blob(&store, &context_bytes);
    assert_eq!(context.to_string(), CONTEXT_OID);

    let end = write_blob(&store, b"4\n");
    assert_eq!(end.to_string(), LINES_END_OID);
    let start = write_blob(&store, b"3\n");
    assert_eq!(start.to_string(), LINES_START_OID);
    let some = write_tree(
        &store,
        vec![
            entry("end", EntryKind::Blob, end),
            entry("start", EntryKind::Blob, start),
        ],
    );
    assert_eq!(some.to_string(), LINES_SOME_OID);
    let lines = write_tree(&store, vec![entry("some", EntryKind::Tree, some)]);
    assert_eq!(lines.to_string(), LINES_OID);

    let path = write_blob(&store, b"file.txt\n");
    assert_eq!(path.to_string(), PATH_OID);

    let identity_root = write_tree(
        &store,
        vec![
            entry("genesis", EntryKind::Blob, genesis_entry),
            entry("lines", EntryKind::Tree, lines),
            entry("path", EntryKind::Blob, path),
        ],
    );
    assert_eq!(identity_root.to_string(), IDENTITY_ROOT);

    let hints_root = write_tree(
        &store,
        vec![
            entry("blob", EntryKind::Blob, blob_entry),
            entry("content", EntryKind::Blob, content),
            entry("context", EntryKind::Blob, context),
        ],
    );
    assert_eq!(hints_root.to_string(), HINTS_ROOT);

    let anchor_root = write_tree(
        &store,
        vec![
            entry("hints", EntryKind::Tree, hints_root),
            entry("identity", EntryKind::Tree, identity_root),
        ],
    );
    assert_eq!(anchor_root.to_string(), ANCHOR_ROOT);

    let root = write_tree(
        &store,
        vec![entry("Position", EntryKind::Tree, anchor_root)],
    );
    (root, store)
}

/// Item 1 + 2 of the fixture contract: every reconstructed object's oid —
/// including the root's — matches the value the current code produces.
/// Content addressing means a single corrupted byte anywhere above fails
/// this assertion (or one of `build_fixture`'s own, reached first).
#[test]
fn reconstructing_the_fixture_reproduces_every_recorded_oid() {
    let (root, _store) = build_fixture();
    assert_eq!(root.to_string(), ROOT);
}

/// Item 3: `Binding::deserialize` decodes the fixture as
/// `Binding::Position`, recovering exactly the `Anchor` the current stored
/// format has always encoded.
#[test]
fn the_fixture_deserializes_as_a_position_binding() {
    let (root, store) = build_fixture();

    let binding = Binding::deserialize(&root, &store).expect("deserialize");
    let Binding::Position(anchor) = binding else {
        panic!("the fixture must decode as Binding::Position");
    };
    assert_eq!(anchor.identity.path, "file.txt");
    assert_eq!(anchor.identity.lines, Some(LineRange { start: 3, end: 4 }));
    assert_eq!(anchor.hints.content, numbered(1..=10).into_bytes());
    assert_eq!(anchor.hints.context, numbered(1..=7).into_bytes());
    assert_eq!(
        gix::ObjectId::from(anchor.hints.blob).to_string(),
        CONTENT_OID
    );
    assert_eq!(
        gix::ObjectId::from(anchor.identity.genesis).to_string(),
        GENESIS_ENTRY_RAW
    );
}

/// Item 4: re-encoding the decoded binding into a fresh store reproduces
/// the fixture's root oid exactly — the existing anchor storage format is
/// unchanged, byte for byte, now that it decodes through `Binding`.
#[test]
fn re_encoding_reproduces_the_fixture_root_byte_for_byte() {
    let (root, store) = build_fixture();
    let binding = Binding::deserialize(&root, &store).expect("deserialize");

    let fresh = ObjectStore::default();
    let re_root = binding.serialize_into(&fresh).expect("serialize");
    assert_eq!(re_root.to_string(), ROOT);
}
