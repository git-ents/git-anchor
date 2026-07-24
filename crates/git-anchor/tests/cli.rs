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
}
