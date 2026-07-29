# git-anchor — dev plan

Extract the anchor subsystem from `../git-ents` into this standalone repo, modeled file-for-file on `../git-store`.
Two published-shape crates: **`gix-anchor`** (library) and **`git-anchor`** (CLI, invoked as `git anchor`).

## Source material

- Library source: `../git-ents/crates/kernel/ents-anchor` (~3.5k lines: `anchor.rs`, `binding.rs`, `diff.rs`, `projection.rs`, `error.rs`, `fixture.rs`, `util.rs`; tests `binding_roundtrip.rs`, `retention.rs`).
  Deps: `facet`, `facet-git-tree`, `gix`, `gix-object`, `thiserror`.
- Spec it implements: `../git-ents/docs/spec/anchor.sdoc` (port the relevant spec
  into this repo's `docs/`; keep the spec-coverage doc comments in `lib.rs` accurate
  after the move).
- Template: `../git-store` — copy conventions, configs, and workflows verbatim,
  adapting names.

## Phase 1 — Scaffold from git-store

Copy from `../git-store`, renaming `store` → `anchor` everywhere:

- `.config/` — `committed.toml`, `deny.toml`, `pre-commit.yaml`, `rumdl.toml`,
  `typos.toml`, `release-please-config.json` + `release-please-manifest.json`
  (crate paths updated, versions reset).
- `.github/workflows/` — `CI.yml`, `CD.yml`, `Lint.yml`, `Version.yml` (update
  binary/crate names in CD).
- `.gitignore`, `.rules`, `.zed/settings.json`, `CONDUCT.md`, `CONTRIBUTING.md`,
  `COPYRIGHT`, `LICENSE-APACHE`, `LICENSE-MIT`.
- Root `Cargo.toml`: workspace, `resolver = "3"`, members
  `crates/gix-anchor`, `crates/git-anchor`, `crates/test-support`.
- `crates/test-support`: copy from git-store as-is (trim to what tests use).

Match git-store's manifest style: edition 2024, same `gix` version/features (`0.85.0`, `default-features = false`, `sha1`, `zlib-rs`; CLI adds `interrupt`, `revision`), same `facet` (`0.50.0-rc.0`, `reflect`) and `facet-git-tree` versions.
**Decision:** `facet-git-tree` lives in git-store as a path crate — use the published crates.io version if available; otherwise a git dependency on git-store.
Do not vendor it.

## Phase 2 — `crates/gix-anchor` (library)

Port `ents-anchor` → `gix-anchor`:

- Rename crate + all `ents_anchor` paths; strip git-ents-workspace inheritance
  (`edition.workspace` etc.) to match git-store's per-crate manifests.
- Remove/replace references to `ents-forge`/git-ents internals in docs and
  doctests (the `Comment`-shaped stand-in struct pattern already used there is
  fine to keep).
- Keep the existing model intact: `Anchor` (blob + optional `LineRange` +
  commit), `capture`/`capture_worktree`, `snippet`, `project`/`project_exact`/
  `project_from_context`/`project_worktree`, the four-outcome `Projection`,
  `diff_trees`, and the binding (serialization via `facet-git-tree`).
- **Generalize per this repo's charter** ("attach arbitrary content to Git objects, optionally line ranges within a blob"): the anchor *target* becomes any object id (commit, tree, tag, or blob; line ranges valid only for blobs), and the attached *payload* is any `Facet` type serialized with `facet-git-tree`.
  Design this as an additive layer over the ported code — do not rewrite projection to do it.
- Port both integration tests + proptests; all doctests runnable.

## Phase 3 — Persistence

Anchors must survive in the repo.
Follow `gix-store`'s pattern (`store.rs`/`refname.rs`): serialized anchor trees reachable under a ref namespace, e.g. `refs/anchors/<target-oid>/<anchor-id>`.
**Decision:** depend on `gix-store` (git dep on git-store) vs. reimplement the small ref-store layer here.
Recommend depending on it — the user said to *use* git-store, and it keeps retention (`anchor.retention`: content stored as real blobs, never gitlinks) in one place.

**What actually shipped, and the correction:** this recommendation was not followed — `store.rs`/`refname.rs` reimplement per-ref locking, CAS retry, and refname validation locally, taking only `facet-git-tree` from git-store.
`../git-store` has since factored exactly that layer out as `gix-refstore`, and `DEVPLAN-attest.md` Phase 0 migrated this crate onto it (done 2026-07-29), so the family has one ref-CAS engine rather than two.
Treat that phase, not this line, as the record of what shipped.

## Phase 4 — `crates/git-anchor` (CLI)

Mirror git-store's CLI crate (`clap` derive, `anyhow`, thin `main.rs`).
Subcommands (installable as `git anchor` via binary name on PATH):

- `git anchor add <object> [-L <start>,<end>] [--path <blob-path>] (-m <msg> | -F <file> | --json <file>)` — capture + store.
- `git anchor list [<object>]`
- `git anchor show <anchor-id> [--json]`
- `git anchor project <anchor-id> <commit>` — print projection outcome + snippet.
- `git anchor remove <anchor-id>`

Exit codes and output style copied from git-store's CLI.
Integration tests with `test-support` + `tempfile` fixtures per subcommand.

## Phase 5 — Docs & polish

- Root `README.md` + per-crate READMEs in git-store's style (feature table if
  any features exist).
- `docs/specification.adoc` — port/adapt `anchor.sdoc` content to this repo's
  doc format (git-store uses asciidoc).
- `cargo fmt`, `cargo clippy` clean, `cargo deny check`, typos/rumdl/committed
  clean per `.config`; CI workflows green.

## Definition of done

- `cargo test --workspace` passes; doctests included.
- `git anchor add/list/show/project/remove` round-trip against a scratch repo.
- Repo file tree is a believable sibling of git-store (same top-level files).
- No references to `ents-*` remain except attribution in docs where useful.

## Open decisions (recommendations inline above)

1. `facet-git-tree` dependency source (published vs. git dep on git-store).
   Resolved: git dep on git-store.
2. Depend on `gix-store` for ref persistence vs. local minimal ref layer.
   Resolved late: shipped as a local layer, then migrated onto `gix-refstore` — see Phase 3's correction above.
3. Ref namespace layout for stored anchors (`refs/anchors/...`).
   Resolved: `refs/anchors/<target-hex>/<id-hex>`, prefix-configurable via `Store::with_prefix`.
