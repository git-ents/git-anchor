# git-comment (Zed extension)

Wires Zed's language-server machinery to [`git comment lsp`](../../crates/git-comment) (`gix-comment-lsp`), an editor-facing, read-time view over `refs/comments/*`: code lenses on anchored comments, hover threads, hint diagnostics, and a compose-on-save flow for new comments and replies.
See [`gix-comment-lsp`](../../crates/gix-comment-lsp) for how the protocol surface itself works.

This extension is a thin binding only: it locates the `git-comment` binary on `PATH` and runs `git-comment lsp`.
Every derivation — listing, projection, rendering — lives in `gix-comment-lsp`, so the behavior is identical to any other LSP client driving the same binary.

## Requirements

`git-comment` must be on `PATH`.
From a `git-anchor` checkout:

```console
cargo install --path crates/git-comment
```

## Installing as a dev extension

1. Open Zed.
2. Run the `zed: extensions` command from the command palette.
3. Click **Install Dev Extension** and select this directory (`editors/zed`).

Zed builds the extension to WebAssembly itself; you'll need the `wasm32-wasip2` target installed:

```console
rustup target add wasm32-wasip2
```

## Language coverage

Zed extensions attach a language server to a fixed list of languages, declared in `extension.toml`.
This extension lists a broad set of common languages up front; if yours is missing, add its Zed language name to `[language_servers.git-comment].languages` in `extension.toml` and reinstall the dev extension.
