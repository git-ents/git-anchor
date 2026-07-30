//! Drive the built `git-anchor` binary against a temp repo, exactly as
//! `git anchor …` would.
//!
//! `git-anchor` defines no document type of its own, so every test here
//! first publishes a schema for some test fixture kind — the same thing any
//! `gix-store` consumer would already have done before a user ever runs
//! `git anchor`.

use std::path::Path;
use std::process::Command;

use facet::Facet;
use gix_anchor::Binding;
use gix_store::{Layout, RefPrefix, RefSegment, RepoStore};
use test_support::init_repo;

const BIN: &str = env!("CARGO_BIN_EXE_git-anchor");

/// A minimal anchorable document: a binding plus one required `String`
/// field — enough to exercise `inject`'s positional-argument rule end to
/// end.
#[derive(Facet)]
struct Doc {
    binding: Binding,
    body: String,
}

/// An anchorable document with two required `String` fields besides the
/// binding — `inject`'s positional argument must refuse as ambiguous rather
/// than guess which one it means.
#[derive(Facet)]
struct TwoStrings {
    binding: Binding,
    a: String,
    b: String,
}

/// An anchorable document with a required field no positional argument can
/// ever fill (not `String`) — only `--json` can complete it.
#[derive(Facet)]
struct WithCount {
    binding: Binding,
    count: u64,
}

/// A document with no `Binding` field at all — not anchorable.
#[derive(Facet)]
struct Plain {
    text: String,
}

/// `range.map(|n| "line {n}\n")` concatenated — a small multi-line fixture
/// file to anchor into.
fn numbered(range: std::ops::RangeInclusive<u32>) -> String {
    range.map(|n| format!("line {n}\n")).collect()
}

/// Stage everything in `dir` and commit it under a fixed test identity.
fn commit_all(dir: &Path, message: &str) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["add", "-A"])
        .status()
        .unwrap();
    assert!(status.success());
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "-m",
            message,
        ])
        .status()
        .unwrap();
    assert!(status.success());
}

/// Run the binary in `dir`, returning `(stdout, stderr, ok)`.
fn run(dir: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(BIN)
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// `create` with the given args, returning the printed capture handle
/// (panics on failure — every test's `create` call is expected to succeed).
fn create(dir: &Path, args: &[&str]) -> String {
    let full: Vec<&str> = std::iter::once("create")
        .chain(args.iter().copied())
        .collect();
    let (out, err, ok) = run(dir, &full);
    assert!(ok, "create failed: {err}");
    out.trim().to_owned()
}

/// `id <handle>`, returning the printed anchor id (panics on failure).
fn anchor_id(dir: &Path, handle: &str) -> String {
    let (out, err, ok) = run(dir, &["id", handle]);
    assert!(ok, "id failed: {err}");
    out.trim().to_owned()
}

fn setup(path: &Path) {
    init_repo(path);
    std::fs::write(path.join("file.txt"), numbered(1..=10)).unwrap();
    commit_all(path, "one");
}

/// The `{data, schema}` [`Layout`] `git-anchor` itself derives from a
/// `--prefix` value — duplicated here (rather than depending on the binary's
/// internals) since a fixture publisher is exactly the kind of independent
/// `gix-store` consumer `git anchor` is meant to work with.
fn layout(prefix: &str) -> Layout {
    let prefix = RefPrefix::new(prefix).unwrap();
    Layout {
        data: prefix.child(&RefSegment::new("data").unwrap()),
        schema: prefix.child(&RefSegment::new("schema").unwrap()),
    }
}

fn publish<T: for<'a> Facet<'a>>(repo_path: &Path, prefix: &str, kind: &str) {
    let repo = gix::open(repo_path).unwrap();
    let store = RepoStore::open_with_layout(&repo, layout(prefix));
    store
        .kind::<T>(RefSegment::new(kind).unwrap())
        .publish()
        .unwrap();
}

// ── create: content-addressed, dedups ───────────────────────────────────

#[test]
fn create_with_identical_coordinates_and_state_prints_the_identical_handle() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    let first = create(path, &["--path", "file.txt", "-L", "2,4"]);
    let second = create(path, &["--path", "file.txt", "-L", "2,4"]);
    assert_eq!(first, second);
    assert!(!first.is_empty());
}

/// Identity is now byte-precise (ARCHITECTURE.md's "Identity rule"): a
/// committed capture and a `--worktree` capture of *byte-identical* content
/// at the same commit and path resolve to the identical anchor id — and,
/// since a fingerprint hint is a pure function of those same bytes, the
/// identical handle too. Capturing after an edit that changes the anchored
/// span's own bytes is a *different* anchor, by design: only edits strictly
/// outside the span leave the span's byte offsets (and so its identity)
/// unchanged.
#[test]
fn committed_and_worktree_captures_of_the_same_bytes_share_a_handle() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    let committed = create(path, &["--path", "file.txt", "-L", "5,6"]);
    let worktree = create(path, &["--path", "file.txt", "-L", "5,6", "--worktree"]);
    assert_eq!(
        committed, worktree,
        "byte-identical capture, same commit and path: identical handle"
    );
    assert_eq!(anchor_id(path, &committed), anchor_id(path, &worktree));

    let (_, err, ok) = run(
        path,
        &["create", "--path", "file.txt", "--worktree", "--at", "HEAD"],
    );
    assert!(!ok, "--worktree with --at should fail");
    assert!(
        err.contains("worktree") || err.contains("cannot be used"),
        "stderr: {err}"
    );
}

#[test]
fn any_identity_coordinate_change_changes_the_anchor_id() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    let base = anchor_id(path, &create(path, &["--path", "file.txt", "-L", "2,4"]));

    let different_lines = anchor_id(path, &create(path, &["--path", "file.txt", "-L", "3,5"]));
    assert_ne!(base, different_lines);

    std::fs::write(path.join("other.txt"), numbered(1..=10)).unwrap();
    commit_all(path, "two");
    let different_path = anchor_id(path, &create(path, &["--path", "other.txt", "-L", "2,4"]));
    assert_ne!(base, different_path);

    // "two" left file.txt itself unchanged, so this capture shares `base`'s
    // path and lines and differs only in genesis (now the "two" commit).
    let different_genesis = anchor_id(path, &create(path, &["--path", "file.txt", "-L", "2,4"]));
    assert_ne!(base, different_genesis);
}

#[test]
fn no_path_and_lines_together_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    let (_, err, ok) = run(path, &["create", "-L", "1,2"]);
    assert!(!ok);
    assert!(err.contains("--path"), "stderr: {err}");
    assert!(err.contains("-L"), "stderr: {err}");
}

#[test]
fn lines_accepts_git_log_forms_start_plus_count_and_embedded_path() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    let (out, err, ok) = run(path, &["create", "--path", "file.txt", "-L", "2,+2"]);
    assert!(ok, "create with start+count failed: {err}");
    assert!(!out.trim().is_empty());

    let (_, err, ok) = run(
        path,
        &["create", "--path", "file.txt", "-L", "2,3:other.txt"],
    );
    assert!(!ok, "disagreeing paths should fail");
    assert!(err.contains("disagree"), "stderr: {err}");
}

// ── inject: the reflection-based field-population rule ──────────────────

#[test]
fn inject_fills_binding_by_reflection_and_text_by_the_lone_string_field() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();
    publish::<Doc>(path, "refs/anchors", "doc");
    let handle = create(path, &["--path", "file.txt", "-L", "2,4"]);

    let (out, err, ok) = run(path, &["inject", "doc", "hello world", "--anchor", &handle]);
    assert!(ok, "inject failed: {err}");
    let name = out.trim().to_owned();
    assert!(!name.is_empty());

    let (out, err, ok) = run(path, &["show", "doc", &name, "--json"]);
    assert!(ok, "show --json failed: {err}");
    assert!(
        out.contains("\"body\":\"hello world\""),
        "show --json: {out}"
    );
    assert!(out.contains("\"Position\""), "show --json: {out}");
}

#[test]
fn inject_accepts_a_whole_commit_binding() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();
    publish::<Doc>(path, "refs/anchors", "doc");
    let handle = create(path, &[]);

    let (out, err, ok) = run(
        path,
        &["inject", "doc", "whole commit", "--anchor", &handle],
    );
    assert!(ok, "inject failed: {err}");
    let name = out.trim().to_owned();

    let (out, err, ok) = run(path, &["show", "doc", &name, "--json"]);
    assert!(ok, "show failed: {err}");
    assert!(out.contains("\"Commit\""), "show --json: {out}");
    assert!(out.contains("whole commit"), "show --json: {out}");
}

#[test]
fn inject_refuses_a_kind_with_no_binding_field() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();
    publish::<Plain>(path, "refs/anchors", "plain");
    let handle = create(path, &[]);

    let (_, err, ok) = run(path, &["inject", "plain", "hello", "--anchor", &handle]);
    assert!(!ok, "inject on a non-anchorable kind should refuse");
    assert!(err.contains("not anchorable"), "stderr: {err}");
}

#[test]
fn inject_refuses_an_ambiguous_positional_argument() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();
    publish::<TwoStrings>(path, "refs/anchors", "two");
    let handle = create(path, &[]);

    let (_, err, ok) = run(path, &["inject", "two", "which one", "--anchor", &handle]);
    assert!(!ok, "inject should refuse with two String candidates");
    assert!(err.contains('a') && err.contains('b'), "stderr: {err}");
}

#[test]
fn inject_refuses_required_fields_it_cannot_fill_from_the_command_line() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();
    publish::<WithCount>(path, "refs/anchors", "counted");
    let handle = create(path, &[]);

    let (_, err, ok) = run(path, &["inject", "counted", "--anchor", &handle]);
    assert!(!ok, "inject should refuse an unfillable required field");
    assert!(err.contains("count"), "stderr: {err}");
}

#[test]
fn inject_json_supplies_the_document_and_still_gets_the_binding_injected() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();
    publish::<WithCount>(path, "refs/anchors", "counted");
    let handle = create(path, &[]);

    let (out, err, ok) = run(
        path,
        &[
            "inject",
            "counted",
            "--json",
            "{\"count\": 5}",
            "--anchor",
            &handle,
        ],
    );
    assert!(ok, "inject --json failed: {err}");
    let name = out.trim().to_owned();

    let (out, err, ok) = run(path, &["show", "counted", &name, "--json"]);
    assert!(ok, "show --json failed: {err}");
    assert!(out.contains("\"count\":5"), "show --json: {out}");
    assert!(out.contains("\"Commit\""), "show --json: {out}");
}

/// `inject` groups by the anchor id: a committed capture and a
/// `--worktree` capture of byte-identical content at the same commit and
/// path land in the same group.
#[test]
fn committed_and_worktree_captures_inject_into_the_same_group() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();
    publish::<Doc>(path, "refs/anchors", "doc");

    let committed_handle = create(path, &["--path", "file.txt", "-L", "5,6"]);
    let expected_group = anchor_id(path, &committed_handle);

    let (out, err, ok) = run(
        path,
        &[
            "inject",
            "doc",
            "committed note",
            "--anchor",
            &committed_handle,
        ],
    );
    assert!(ok, "inject failed: {err}");
    let committed_name = out.trim().to_owned();

    let worktree_handle = create(path, &["--path", "file.txt", "-L", "5,6", "--worktree"]);
    let (out, err, ok) = run(
        path,
        &[
            "inject",
            "doc",
            "worktree note",
            "--anchor",
            &worktree_handle,
        ],
    );
    assert!(ok, "inject --worktree failed: {err}");
    let worktree_name = out.trim().to_owned();

    let committed_group = committed_name.split('/').next().unwrap();
    let worktree_group = worktree_name.split('/').next().unwrap();
    assert_eq!(committed_group, expected_group);
    assert_eq!(committed_group, worktree_group);
}

// ── list / show / remove, generic over the kind ─────────────────────────

#[test]
fn list_and_remove_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();
    publish::<Doc>(path, "refs/anchors", "doc");
    let handle = create(path, &[]);

    let (out, _, ok) = run(path, &["inject", "doc", "first", "--anchor", &handle]);
    assert!(ok);
    let name = out.trim().to_owned();

    let (out, err, ok) = run(path, &["list", "doc"]);
    assert!(ok, "list failed: {err}");
    assert!(out.contains(&name), "list output: {out}");
    assert!(out.contains("first"), "list output: {out}");

    let (out, err, ok) = run(path, &["list", "doc", "--json"]);
    assert!(ok, "list --json failed: {err}");
    assert!(
        out.contains(&format!("\"name\":\"{name}\"")),
        "list --json: {out}"
    );

    let (_, err, ok) = run(path, &["remove", "doc", &name]);
    assert!(ok, "remove failed: {err}");

    let (out, _, ok) = run(path, &["list", "doc"]);
    assert!(ok);
    assert_eq!(out.trim(), "", "doc should be empty after removal: {out}");

    let (_, err, ok) = run(path, &["remove", "doc", &name]);
    assert!(!ok, "second removal should fail");
    assert!(err.contains("no entity"), "stderr: {err}");
}

#[test]
fn remove_is_atomic_on_a_bad_name() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();
    publish::<Doc>(path, "refs/anchors", "doc");
    let handle = create(path, &[]);

    let (out, _, ok) = run(path, &["inject", "doc", "one", "--anchor", &handle]);
    assert!(ok);
    let name = out.trim().to_owned();

    let (out_before, _, _) = run(path, &["list", "doc"]);
    let (_, err, ok) = run(path, &["remove", "doc", &name, "not/a-real-name"]);
    assert!(!ok, "remove with a bad name should fail atomically");
    assert!(err.contains("no entity"), "stderr: {err}");
    let (out_after, _, _) = run(path, &["list", "doc"]);
    assert_eq!(
        out_before, out_after,
        "a failed multi-remove must not remove anything"
    );
}

#[test]
fn show_reports_no_entity_for_an_unknown_name() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();
    publish::<Doc>(path, "refs/anchors", "doc");

    let (_, err, ok) = run(
        path,
        &[
            "show",
            "doc",
            "0000000000000000000000000000000000000000/1111111111111111111111111111111111111111",
        ],
    );
    assert!(!ok);
    assert!(err.contains("no entity"), "stderr: {err}");
}

// ── bare invocation: list registered kinds ──────────────────────────────

#[test]
fn bare_invocation_lists_kinds_and_marks_anchorable_ones() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();
    publish::<Doc>(path, "refs/anchors", "doc");
    publish::<Plain>(path, "refs/anchors", "plain");

    let (out, err, ok) = run(path, &[]);
    assert!(ok, "bare invocation failed: {err}");
    assert!(out.contains("doc  (anchorable)"), "kinds output: {out}");
    assert!(out.contains("plain"), "kinds output: {out}");
    assert!(!out.contains("plain  (anchorable)"), "kinds output: {out}");
}

// ── show: an entity exactly as stored, no resolution ─────────────────────
//
// `show` no longer resolves a position binding onto another revision or the
// working tree (ARCHITECTURE.md: "`project` is library-internal... no
// user-facing command resolves through it"); that is `git-query`'s
// `bind/5`, not this CLI's job.

#[test]
fn show_prints_an_injected_entity_as_stored() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();
    publish::<Doc>(path, "refs/anchors", "doc");
    let handle = create(path, &["--path", "file.txt", "-L", "3,4"]);

    let (out, err, ok) = run(path, &["inject", "doc", "note", "--anchor", &handle]);
    assert!(ok, "inject failed: {err}");
    let name = out.trim().to_owned();

    let (out, err, ok) = run(path, &["show", "doc", &name, "--json"]);
    assert!(ok, "show failed: {err}");
    assert!(out.contains("note"), "show output: {out}");
}

#[test]
fn worktree_capture_injects_and_shows_as_stored() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();
    publish::<Doc>(path, "refs/anchors", "doc");

    let handle = create(path, &["--path", "file.txt", "-L", "5,6", "--worktree"]);
    let (out, err, ok) = run(
        path,
        &["inject", "doc", "worktree note", "--anchor", &handle],
    );
    assert!(ok, "inject --worktree failed: {err}");
    let name = out.trim().to_owned();

    let (out, err, ok) = run(path, &["show", "doc", &name, "--json"]);
    assert!(ok, "show failed: {err}");
    assert!(out.contains("worktree note"), "show output: {out}");
}

// ── the ref namespace is a plain argument, not hard-coded ────────────────

#[test]
fn prefix_selects_a_disjoint_store() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();
    publish::<Doc>(path, "refs/example", "doc");
    let handle = create(path, &[]);

    let (out, err, ok) = run(
        path,
        &[
            "--prefix",
            "refs/example",
            "inject",
            "doc",
            "elsewhere",
            "--anchor",
            &handle,
        ],
    );
    assert!(ok, "inject under a custom prefix failed: {err}");
    let name = out.trim().to_owned();

    // The default prefix (`refs/anchors`) never published this kind, so it
    // lists as empty rather than erroring — `list` reads the ref namespace
    // directly and does not require a published schema to try.
    let (out, err, ok) = run(path, &["list", "doc"]);
    assert!(ok, "list under the default prefix failed: {err}");
    assert_eq!(
        out.trim(),
        "",
        "the default prefix should not see the custom one: {out}"
    );

    let (out, err, ok) = run(path, &["--prefix", "refs/example", "show", "doc", &name]);
    assert!(ok, "show under a custom prefix failed: {err}");
    assert!(out.contains("elsewhere"), "show output: {out}");
}
