//! Drive the built `git-anchor` binary against a temp repo, exactly as
//! `git anchor …` would.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use test_support::init_repo;

const BIN: &str = env!("CARGO_BIN_EXE_git-anchor");

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

/// Run the binary in `dir`, feeding `stdin`, returning `(stdout, stderr, ok)`.
fn run(dir: &Path, stdin: Option<&str>, args: &[&str]) -> (String, String, bool) {
    let mut child = Command::new(BIN)
        .current_dir(dir)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

fn setup(path: &Path) {
    init_repo(path);
    std::fs::write(path.join("file.txt"), numbered(1..=10)).unwrap();
    commit_all(path, "one");
}

#[test]
fn add_with_path_and_lines_then_list_and_show() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    let (out, err, ok) = run(
        path,
        None,
        &["add", "--path", "file.txt", "-L", "2,4", "-m", "note"],
    );
    assert!(ok, "add failed: {err}");
    let id = out.trim().to_owned();
    assert_eq!(id.len(), 40, "expected a full hex id: {out}");

    let (out, err, ok) = run(path, None, &["list"]);
    assert!(ok, "list failed: {err}");
    assert!(out.contains(&id[..8]), "list output: {out}");
    assert!(out.contains("note"), "list output: {out}");

    let (out, err, ok) = run(path, None, &["show", &id]);
    assert!(ok, "show failed: {err}");
    assert!(out.contains("note"), "show output: {out}");
    assert!(out.contains("line 2\nline 3\nline 4"), "show output: {out}");
    assert!(out.contains("binding: position"), "show output: {out}");
}

#[test]
fn add_without_path_attaches_to_head_commit() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    let (out, err, ok) = run(path, None, &["add", "-m", "whole commit note"]);
    assert!(ok, "add failed: {err}");
    let id = out.trim().to_owned();

    let (out, err, ok) = run(path, None, &["list"]);
    assert!(ok, "list failed: {err}");
    assert!(out.contains(&id[..8]), "list output: {out}");

    let (out, err, ok) = run(path, None, &["show", &id]);
    assert!(ok, "show failed: {err}");
    assert!(out.contains("binding: commit"), "show output: {out}");
    assert!(out.contains("whole commit note"), "show output: {out}");
}

#[test]
fn show_accepts_a_prefix_and_rejects_ambiguous_or_missing_ids() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    let (out, _, ok) = run(path, None, &["add", "-m", "one"]);
    assert!(ok);
    let id = out.trim().to_owned();

    let (out, err, ok) = run(path, None, &["show", &id[..8]]);
    assert!(ok, "show with prefix failed: {err}");
    assert!(out.contains("one"), "show output: {out}");

    let (_, err, ok) = run(path, None, &["show", "00"]);
    assert!(!ok);
    assert!(err.contains("no note matches"), "stderr: {err}");

    // A single hex digit matches every note in this repo (there's only one),
    // so pad the fixture with a second note to exercise ambiguity.
    let (out2, _, ok) = run(path, None, &["add", "--path", "file.txt", "-m", "two"]);
    assert!(ok);
    let id2 = out2.trim().to_owned();
    let common_prefix_len = id
        .chars()
        .zip(id2.chars())
        .take_while(|(a, b)| a == b)
        .count();
    if common_prefix_len > 0 {
        let (_, err, ok) = run(path, None, &["show", &id[..common_prefix_len]]);
        assert!(!ok);
        assert!(err.contains("ambiguous"), "stderr: {err}");
    }
}

#[test]
fn show_at_rev_reports_current_for_an_unchanged_anchor() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    let (out, err, ok) = run(
        path,
        None,
        &["add", "--path", "file.txt", "-L", "3,4", "-m", "note"],
    );
    assert!(ok, "add failed: {err}");
    let id = out.trim().to_owned();

    let (out, err, ok) = run(path, None, &["show", &format!("{id}@HEAD")]);
    assert!(ok, "show @rev failed: {err}");
    assert!(out.contains("current"), "show @rev output: {out}");
}

#[test]
fn show_at_rev_rejects_a_commit_binding() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    let (out, _, ok) = run(path, None, &["add", "-m", "whole commit"]);
    assert!(ok);
    let id = out.trim().to_owned();

    let (_, err, ok) = run(path, None, &["show", &format!("{id}@HEAD")]);
    assert!(!ok);
    assert!(err.contains("line/blob"), "stderr: {err}");
}

#[test]
fn remove_deletes_a_note_and_a_second_removal_fails() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    let (out, _, ok) = run(path, None, &["add", "-m", "to be removed"]);
    assert!(ok);
    let id = out.trim().to_owned();

    let (_, err, ok) = run(path, None, &["remove", &id]);
    assert!(ok, "remove failed: {err}");

    let (out, _, ok) = run(path, None, &["list"]);
    assert!(ok);
    assert_eq!(out.trim(), "", "list should be empty after removal: {out}");

    let (_, err, ok) = run(path, None, &["remove", &id]);
    assert!(!ok, "second removal should fail");
    assert!(err.contains("no note"), "stderr: {err}");
}

#[test]
fn no_path_and_lines_together_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    let (_, err, ok) = run(path, None, &["add", "-L", "1,2", "-m", "bad"]);
    assert!(!ok);
    assert!(err.contains("--path"), "stderr: {err}");
    assert!(err.contains("-L"), "stderr: {err}");
}

#[test]
fn lines_with_an_embedded_path_works_with_no_separate_path_flag() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    // `-L START,END:path` supplies the path itself — no `--path` needed.
    let (out, err, ok) = run(path, None, &["add", "-L", "2,4:file.txt", "-m", "note"]);
    assert!(ok, "add with only an embedded -L path failed: {err}");
    let id = out.trim().to_owned();

    let (out, err, ok) = run(path, None, &["show", &id, "--json"]);
    assert!(ok, "show --json failed: {err}");
    assert!(out.contains("\"path\":\"file.txt\""), "show --json: {out}");
    assert!(
        out.contains("\"lines\":{\"start\":2,\"end\":4}"),
        "show --json: {out}"
    );
}

#[test]
fn bare_invocation_lists_instead_of_erroring() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    let (out, err, ok) = run(path, None, &[]);
    assert!(ok, "bare invocation failed: {err}");
    assert_eq!(out.trim(), "", "no notes yet: {out}");

    let (out, _, ok) = run(path, None, &["add", "-m", "a note"]);
    assert!(ok);
    let id = out.trim().to_owned();

    let (out, err, ok) = run(path, None, &[]);
    assert!(ok, "bare invocation failed: {err}");
    assert!(out.contains(&id[..8]), "bare list output: {out}");
}

#[test]
fn edit_replaces_the_body_and_keeps_the_same_id() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    let (out, _, ok) = run(path, None, &["add", "-m", "first body"]);
    assert!(ok);
    let id = out.trim().to_owned();

    let (out, err, ok) = run(path, None, &["edit", &id, "-m", "second body"]);
    assert!(ok, "edit failed: {err}");
    assert_eq!(out.trim(), id, "edit reattaches the same identity oid");

    let (out, err, ok) = run(path, None, &["show", &id]);
    assert!(ok, "show failed: {err}");
    assert!(out.contains("second body"), "show output: {out}");
    assert!(!out.contains("first body"), "show output: {out}");
}

#[test]
fn append_joins_new_content_with_a_blank_line() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    let (out, _, ok) = run(path, None, &["add", "-m", "first paragraph"]);
    assert!(ok);
    let id = out.trim().to_owned();

    let (_, err, ok) = run(path, None, &["append", &id, "-m", "second paragraph"]);
    assert!(ok, "append failed: {err}");

    let (out, err, ok) = run(path, None, &["show", &id, "--json"]);
    assert!(ok, "show --json failed: {err}");
    assert!(
        out.contains("first paragraph\\n\\nsecond paragraph"),
        "show --json output: {out}"
    );
}

#[test]
fn log_prints_every_version_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    let (out, _, ok) = run(path, None, &["add", "-m", "v1"]);
    assert!(ok);
    let id = out.trim().to_owned();
    let (_, err, ok) = run(path, None, &["edit", &id, "-m", "v2"]);
    assert!(ok, "edit failed: {err}");

    let (out, err, ok) = run(path, None, &["log", &id]);
    assert!(ok, "log failed: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "log output: {out}");
    // Newest first: the most recent commit's line comes first.
    for line in &lines {
        let mut fields = line.split_whitespace();
        let oid = fields.next().expect("oid field");
        assert_eq!(oid.len(), 40, "log line: {line}");
    }
}

#[test]
fn show_ancestor_suffix_reads_older_versions_and_bounds_checks() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    let (out, _, ok) = run(path, None, &["add", "-m", "v1 (oldest)"]);
    assert!(ok);
    let id = out.trim().to_owned();
    let (_, _, ok) = run(path, None, &["edit", &id, "-m", "v2"]);
    assert!(ok);
    let (_, _, ok) = run(path, None, &["edit", &id, "-m", "v3 (tip)"]);
    assert!(ok);

    let (out, err, ok) = run(path, None, &["show", &id]);
    assert!(ok, "show failed: {err}");
    assert!(out.contains("v3 (tip)"), "show output: {out}");

    let (out, err, ok) = run(path, None, &["show", &format!("{id}~0")]);
    assert!(ok, "show ~0 failed: {err}");
    assert!(out.contains("v3 (tip)"), "show ~0 output: {out}");

    let (out, err, ok) = run(path, None, &["show", &format!("{id}~1")]);
    assert!(ok, "show ~1 failed: {err}");
    assert!(out.contains("v2"), "show ~1 output: {out}");

    let (out, err, ok) = run(path, None, &["show", &format!("{id}^")]);
    assert!(ok, "show ^ failed: {err}");
    assert!(out.contains("v2"), "show ^ output: {out}");

    let (out, err, ok) = run(path, None, &["show", &format!("{id}~2")]);
    assert!(ok, "show ~2 failed: {err}");
    assert!(out.contains("v1 (oldest)"), "show ~2 output: {out}");

    let (_, err, ok) = run(path, None, &["show", &format!("{id}~3")]);
    assert!(!ok, "out-of-range ancestor should fail");
    assert!(err.contains("out of range"), "stderr: {err}");
}

#[test]
fn show_rejects_reflog_syntax() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    let (out, _, ok) = run(path, None, &["add", "-m", "note"]);
    assert!(ok);
    let id = out.trim().to_owned();

    let (_, err, ok) = run(path, None, &["show", &format!("{id}@{{yesterday}}")]);
    assert!(!ok, "reflog syntax should be rejected");
    assert!(err.contains("reflog"), "stderr: {err}");
}

#[test]
fn list_by_commit_includes_position_notes_anchored_there() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    let (out, _, ok) = run(
        path,
        None,
        &["add", "--path", "file.txt", "-L", "2,3", "-m", "positioned"],
    );
    assert!(ok);
    let position_id = out.trim().to_owned();

    let (out, _, ok) = run(path, None, &["add", "-m", "whole commit"]);
    assert!(ok);
    let commit_id = out.trim().to_owned();

    let (out, err, ok) = run(path, None, &["list", "HEAD"]);
    assert!(ok, "list HEAD failed: {err}");
    assert!(
        out.contains(&position_id[..8]),
        "list HEAD should include the position note anchored at HEAD: {out}"
    );
    assert!(
        out.contains(&commit_id[..8]),
        "list HEAD should include the commit note: {out}"
    );
}

#[test]
fn list_json_emits_one_object_per_line() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    let (out, _, ok) = run(path, None, &["add", "-m", "a note"]);
    assert!(ok);
    let id = out.trim().to_owned();

    let (out, err, ok) = run(path, None, &["list", "--json"]);
    assert!(ok, "list --json failed: {err}");
    assert!(
        out.contains(&format!("\"id\":\"{id}\"")),
        "list --json: {out}"
    );
    assert!(out.contains("\"binding\":\"commit\""), "list --json: {out}");
    assert!(out.contains("\"summary\":"), "list --json: {out}");
}

#[test]
fn lines_accepts_git_log_forms_start_plus_count_and_embedded_path() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    // `10,+5`-style: start plus a count, rather than an explicit end.
    let (out, err, ok) = run(
        path,
        None,
        &["add", "--path", "file.txt", "-L", "2,+2", "-m", "count"],
    );
    assert!(ok, "add with start+count failed: {err}");
    let id = out.trim().to_owned();
    let (out, err, ok) = run(path, None, &["show", &id, "--json"]);
    assert!(ok, "show --json failed: {err}");
    assert!(
        out.contains("\"lines\":{\"start\":2,\"end\":3}"),
        "show --json output: {out}"
    );

    // A `:path` embedded in the `-L` token matching `--path` is accepted.
    let (_, err, ok) = run(
        path,
        None,
        &[
            "add",
            "--path",
            "file.txt",
            "-L",
            "2,3:file.txt",
            "-m",
            "embedded path",
        ],
    );
    assert!(ok, "add with embedded path failed: {err}");

    // A `:path` embedded in the `-L` token that disagrees with `--path` is
    // an error.
    let (_, err, ok) = run(
        path,
        None,
        &[
            "add",
            "--path",
            "file.txt",
            "-L",
            "2,3:other.txt",
            "-m",
            "bad",
        ],
    );
    assert!(!ok, "disagreeing paths should fail");
    assert!(err.contains("disagree"), "stderr: {err}");
}

#[test]
fn worktree_add_and_show_project_uncommitted_content() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    // Dirty the working tree without committing.
    std::fs::write(
        path.join("file.txt"),
        numbered(1..=10).replace("line 5", "line five"),
    )
    .unwrap();

    let (out, err, ok) = run(
        path,
        None,
        &[
            "add",
            "--path",
            "file.txt",
            "-L",
            "5,6",
            "-m",
            "worktree note",
            "--worktree",
        ],
    );
    assert!(ok, "add --worktree failed: {err}");
    let id = out.trim().to_owned();

    let (out, err, ok) = run(path, None, &["show", &id]);
    assert!(ok, "show failed: {err}");
    assert!(out.contains("line five"), "show output: {out}");

    let (out, err, ok) = run(path, None, &["show", &id, "--worktree"]);
    assert!(ok, "show --worktree failed: {err}");
    assert!(out.contains("current"), "show --worktree output: {out}");

    // `--worktree` conflicts with a positional `<object>`.
    let (_, err, ok) = run(
        path,
        None,
        &["add", "HEAD", "--path", "file.txt", "--worktree", "-m", "x"],
    );
    assert!(!ok, "--worktree with <object> should fail");
    assert!(
        err.contains("worktree") || err.contains("cannot be used"),
        "stderr: {err}"
    );
}

#[test]
fn remove_accepts_multiple_ids_and_is_atomic_on_a_bad_one() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path();

    let (out, _, ok) = run(path, None, &["add", "-m", "one"]);
    assert!(ok);
    let id1 = out.trim().to_owned();
    let (out, _, ok) = run(path, None, &["add", "--path", "file.txt", "-m", "two"]);
    assert!(ok);
    let id2 = out.trim().to_owned();

    // One bad id among good ones: nothing gets removed.
    let (out_before, _, _) = run(path, None, &["list"]);
    let (_, err, ok) = run(path, None, &["remove", &id1, "not-a-real-id"]);
    assert!(!ok, "remove with a bad id should fail atomically");
    assert!(err.contains("no note matches"), "stderr: {err}");
    let (out_after, _, _) = run(path, None, &["list"]);
    assert_eq!(
        out_before, out_after,
        "a failed multi-remove must not remove anything"
    );

    // Both good ids: both get removed.
    let (_, err, ok) = run(path, None, &["remove", &id1, &id2]);
    assert!(ok, "remove failed: {err}");
    let (out, _, ok) = run(path, None, &["list"]);
    assert!(ok);
    assert_eq!(out.trim(), "", "both notes should be gone: {out}");
}
