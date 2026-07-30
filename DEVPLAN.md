# git-anchor — dev plan

[`ARCHITECTURE.md`](ARCHITECTURE.md) states the settled design.
This plan is what remains to make the code match it.
`DEVPLAN-boundary.md` and `DEVPLAN-storage.md` covered the storage-engine and boundary migrations that preceded this one; both shipped and are deleted here — git history retains them.

## Phase 1 — Split `Binding` into `identity` and `hints`

`Binding`'s position case currently carries `commit`, `blob`, `path`, `lines`, `content`, `context` as one flat set of fields, all hashed together into whatever identity a consumer derives from it.
Restructure it into two sibling subtrees:

- `identity` — `genesis_rev`, `path`, `lines` only.
  The anchor id is the content hash of this subtree and nothing else.
- `hints` — `content` (the anchored blob, referenced by its own oid), `context` (the fresh window `project_from_context` falls back to), and room for a future fingerprint/descriptor without a schema migration (`facet-git-tree` writes every field present as a sentinel, so an empty variant costs nothing).

`blob`'s object id becomes derivable — read the genesis tree at `path` — rather than a stored identity field; drop it from `identity`, keep deriving it where a hint needs it.
Every consumer of `Anchor`'s current flat shape (`capture`, `project`, `diff_trees`, `snippet`) moves to the split shape; none of their *behavior* changes, only which fields live where.

Update `docs/specification.adoc`'s `anchor.definition`/`anchor.identity`/`anchor.immutable`/`anchor.retention` requirement ids' doc-comment references in `crates/gix-anchor/src/*.rs` to point at the split fields.

Acceptance: `anchor_id(coords)` is a pure function of `(genesis_rev, path, lines)`; a property test asserts it is invariant under changing `hints` while `identity` is held fixed, and changes when any identity coordinate changes.

## Phase 2 — Comments leave this repo

Another agent deletes `crates/gix-comment`, `crates/git-comment`, `crates/gix-comment-lsp`, and `editors/zed` in parallel with this plan; comments become a `git-forge` document.
Once that lands, confirm nothing here still references them:

- Root `Cargo.toml` workspace members list only `gix-anchor`, `git-anchor`, `test-support`.
- No doc, doctest, or fixture names `gix_comment`/`git_comment`/`gix-comment-lsp`.
- `cargo test --workspace` is green with the smaller member set.

## Phase 3 — `git anchor` narrows to capture + inject

Split the current `add` verb into two:

- `create` — capture only.
  Writes the identity and hints objects to the object database and prints the anchor id.
  Advances no ref; needs no registered kind.
- `inject <kind> [<text>] --anchor <id>` — write an entity of `<kind>` embedding a previously created id, over `gix-store`'s dynamic write path.
  Requires `<kind>`'s published schema to embed `Binding`'s shape, located structurally.

`list`/`show`/`remove` are unaffected in shape; `show <name>@<rev>` still re-derives projection from the entity's own `Binding`.
See [`git-anchor`'s README](crates/git-anchor) for the full command reference this phase implements.

Acceptance: two `create` calls with identical coordinates print the identical id; `inject` with a stale or foreign id still writes (dedup is a property of the id, not a constraint `inject` enforces); `cargo test --workspace` covers both verbs.

## Phase 4 — Dynamic write path moves to `gix-store`

The schema-reflection and dynamic-value-write logic `git anchor inject` needs — locate a schema field structurally equal to `Binding`'s, build a `facet_value::Value` against a runtime `Schema`, write it through `Kind::dynamic` — is generic over any vocabulary type, not anchor-specific.
It belongs in `../git-store` as reusable library surface, not duplicated here as CLI-only code.

Per this project's standing rule (fix substrate gaps upstream in `../git-store` and push, don't work around them here): if `gix-store` is missing a piece this phase needs — a helper for "does this schema embed shape X" as a public function rather than an inlined comparison, for instance — fix it upstream first, then depend on the fixed version.
Do not reimplement it in `crates/git-anchor` a second time.

Acceptance: `crates/git-anchor/src/main.rs` calls into `gix-store` for schema lookup and dynamic writes with no bespoke reflection code of its own; the equivalent logic, if it existed here before this phase, is deleted rather than left as a second copy.

## Definition of done

- `cargo test --workspace` passes, doctests included, over the two-crate workspace.
- `ARCHITECTURE.md`, `docs/specification.adoc`, and both crate READMEs describe exactly what the code does — no more, no less.
- No reference to `gix-comment`/`git-comment`/`gix-comment-lsp` remains anywhere in this repo.
- `git anchor create`/`inject`/`list`/`show`/`remove` round-trip against a scratch repo and a registered test kind.
