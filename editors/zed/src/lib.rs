//! A Zed extension wiring the editor's language-server machinery to
//! `git comment lsp` (`gix-comment-lsp`, on stdio): code lenses, hover
//! threads, hint diagnostics, and the compose-on-save flow for
//! `refs/comments/*`, on whichever of the languages declared in
//! `extension.toml` the current file is.
//!
//! This extension owns no logic of its own beyond locating the
//! `git-comment` binary on `PATH`: every derivation lives in
//! `gix-comment-lsp`, invoked identically for every language and reachable
//! by any editor speaking LSP, not just Zed.

use zed_extension_api::{self as zed, Command, LanguageServerId, Result, Worktree};

struct GitCommentExtension;

impl zed::Extension for GitCommentExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        let command = worktree.which("git-comment").ok_or_else(|| {
            "git-comment not found on PATH. Install it (e.g. `cargo install --path \
             crates/git-comment` from a git-anchor checkout) and make sure it's on PATH."
                .to_string()
        })?;
        Ok(Command {
            command,
            args: vec!["lsp".to_string()],
            env: Vec::new(),
        })
    }
}

zed::register_extension!(GitCommentExtension);
