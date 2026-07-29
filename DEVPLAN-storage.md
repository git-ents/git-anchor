# gix-anchor storage — full `git-store` adoption

`gix-anchor/src/store.rs` still hand-rolls the entity engine: `Note` tree building (`facet_git_tree::serialize_into`/`deserialize` called directly), commit writing (`write_commit`), ref-history walking (`ref_history`), and two identity schemes (binding-keyed `attach`, genesis-keyed `create`/`update`) — all wired directly to `gix-refstore`'s `RefStore`/`Committer`.
That's a storage *engine*, not domain logic, and `../git-store` already has one: `gix-store`'s `Store<R, O>`/`Kind<E, R, O>`.

Goal: `gix-anchor` has **no engine** of its own.
`store.rs` becomes a thin adapter — `Note` as a `Facet` value, translation between `gix-anchor`'s domain operations and `gix_store::Kind` calls — and nothing else.
Every ref CAS retry, commit write, and history walk happens inside `gix-store`.

**No back-compat constraint.**
Neither `gix-anchor` nor `gix-store`'s `Kind` API has a user outside this family of repos yet.
This plan makes breaking changes freely on both sides — ref layout, public Rust API, `gix-store`'s `Kind` signatures — wherever that produces the better design.
There is no existing repo's `refs/anchors/*` data to preserve and no external caller of `Kind`/`Put` to avoid breaking.
`crates/git-anchor` (CLI) and `crates/gix-comment` do depend on `gix_anchor::{Store, RepoStore, StoredNote}` today, but they live in this same repo/workspace — if Phase 2 changes their shape, updating those two call sites is part of the same phase, not a compatibility problem to solve around.

## Why full adoption changes `gix-store`, not just `gix-anchor`

Two mismatches between `gix-store`'s `Kind` abstraction (as it exists at pinned rev `4581dcf`) and what a correctly-designed `gix-anchor` needs.
Both confirmed by reading `gix-store`'s source directly (checked out at `~/.cargo/git/checkouts/git-store-2299406fc43c3cd9/4581dcf/crates/gix-store`):

1. **Entity naming is one flat `RefSegment`.**
   `Kind::put`/`get`/`history`/ `remove` all take `name: &RefSegment` (`kind.rs:70,85,139,149`), and `Kind::reference` builds `<data-prefix>/<kind-name>/<entity-name>` (`kind.rs:56`, `store.rs:17-18`) — two segments total.
   A note's identity is naturally two variable segments — `<target-hex>/<id-hex>` — grouped by target so listing/filtering by target is a ref-prefix scan, not a full-store linear scan.
   Forcing that into `Kind`'s one-flat-name shape means folding `target` into the *kind* name, which drags a **published schema per kind** along with it (`Kind::put` reads `current_schema()` unconditionally, `kind.rs:334`) — one `refs/schema/<target-hex>` per distinct target ever anchored, for a schema (`Note`'s shape) that never actually varies by target.
   That's not a storage detail, it's a wrong entity model for this data.
2. **`Put::anonymous()` truncates the identity to 8 hex chars.**
   It writes the commit first (so its oid is known), then names the ref `RefSegment::new(&commit.to_string()[..8])` (`kind.rs:308-312`) — only the returned `ObjectId` is the full oid; the ref-path segment is not.
   A genesis-keyed note's identity needs full-oid collision odds (the entire reason a genesis identity is "practically unreachable" to collide, rather than merely unlikely) — an 8-hex-char (32-bit) namespace is a real, if small, collision surface for something documented as practically impossible.

Given we own both repos and have no callers to keep working, the right move is to fix `Kind` itself rather than build a workaround in `gix-anchor` for either.

### Phase 0 — Redesign `gix-store`'s `Kind` entity addressing (breaking)

1. **Multi-segment entity names.**
   Replace `Kind`'s `&RefSegment` name parameter (across `put`, `get`, `history`, `remove`, `reference`, `Put::at`) with a type that can express one *or more* segments under the kind — e.g. a small `EntityName` wrapping a non-empty `Vec<RefSegment>`, `From<RefSegment>` for the common single-segment case so existing single-segment call sites (`gix-store`'s own tests) need only `seg("carbonara").into()` or an added blanket impl, and `From<[RefSegment; N]>`/`FromIterator<RefSegment>` for the multi-segment case.
   `Kind::reference` becomes `self.entities.join_path(name)` (add `RefPrefix::join_path` next to the existing single-segment `join`, or fold segments with repeated `.child()`/`.join()` — implementation detail to settle when writing the code).
   This is a breaking signature change to every `Kind` method that takes a name.
   Fix every call site in `gix-store`'s own tests in the same change; there are no other callers.
2. **Full-oid anonymous naming.**
   Change `Put::anonymous()` to name the ref from the full commit oid, not `[..8]`.
   Since the name and the returned `ObjectId` become the same string, simplify the return type too: `anonymous()` returns `ObjectId` alone (the commit), and a caller that needs the `RefSegment`/`EntityName` derives it from that oid the same way `Put::at` callers already build one.
   Drop `Error::NameTaken`'s only remaining trigger (a genuine full-oid collision) down to what it always should have been: unreachable in practice, not a real 32-bit-space failure mode.

Both land in `git-store` as ordinary breaking changes (version bump per that repo's own conventions), fixed up in the same change everywhere they're used (currently: `gix-store`'s own test suite only).

## Phase 1 — Design the entity mapping

- One global `Kind<Typed<Note>, R, O>`, name `"notes"` (or similar — bikeshed
  at implementation time), one schema, published once ever.
  `Note` (currently a private struct in `store.rs`) becomes the `Facet`
  value type handed to `gix_store::Typed<Note>` — same shape (`body`,
  `binding`, `attachment`, `parent`, `state`, `created_at`).
- Schema bootstrap: before the first write through a freshly-opened
  `Store`, check `kind.schema().get()`; call `publish()` only if `None`.
  `KindSchema::put`/`write` always commits forward unconditionally (no
  content-equality short-circuit — confirmed by reading `kind.rs`'s
  `write`), so this must stay a check-then-publish, never an
  unconditional eager `publish()` on every open — that would mint a new,
  useless schema commit on every CLI invocation.
- Entity name: the two-segment `EntityName` from Phase 0 item 1, built from
  `hex_segment(target)` + `hex_segment(id)` — same derivation
  `NoteRef::to_ref_name` uses today.
- `Layout` customization (`gix_store::Layout { data, schema }`): point
  `data` at `refs/anchors` (kept because it is the right, self-descriptive
  name for this tool's namespace, not for compatibility) and `schema` at a
  separate prefix, e.g. `refs/anchors-schema`, so a `refs/anchors` walk
  never needs to filter out the one schema ref.
- Method mapping:
  - `attach`/`attach_with_attachment` → `kind.write(&note).message(summary)
    .at(&name)` (deterministic name from `binding.serialize_into`'s oid,
    same as today).
  - `create` → `kind.write(&note).message(message).anonymous()` (genesis
    identity, full-oid per Phase 0 item 2).
  - `update` → `kind.write(&note).message(message).at(&name)`, `name`
    recovered from the note's existing ref.
  - `list`/`get`/`get_at`/`history`/`remove` → `Kind::list`/`get`/`get_at`/
    `history`/`remove` directly.
  - Looking a note up by identity oid (not by entity name) stays
    `gix-anchor`'s own job — `Kind` addresses entities by name, and this
    crate's callers address notes by identity oid, which is not always the
    same string (binding-keyed: yes; genesis-keyed: yes, now that Phase 0
    item 2 makes the anonymous name the full oid too — worth confirming in
    Phase 2 whether `find_ref`'s linear scan can be dropped entirely once
    both schemes name-by-full-oid).

## Phase 2 — Reimplement `store.rs`

- Add `gix-store` to `crates/gix-anchor/Cargo.toml` (git dependency on `git-store`, same convention as the existing `gix-refstore`/ `facet-git-tree` git dependencies).
  Drop `gix-refstore` as a direct dependency if nothing outside test doubles still needs it directly (re-exported through `gix_store` otherwise); drop `facet_git_tree`'s direct use for `Note` specifically (binding.rs's own use is unrelated domain code and stays).
- Delete from `store.rs`: `write_commit`, `commit_tree`, `with_commit`, `ref_history`, `NoteRef`, `hex_segment`, the inline `facet_git_tree::serialize_into`/`deserialize` calls for `Note`.
  These become `gix-store` internals.
- Keep: `Note` (now the `Typed` value type), `now_nanos`, `StoredNote`,
  the domain method set (`attach`, `attach_with_attachment`, `create`,
  `update`, `list`, `get`, `get_at`, `remove`, `history`) — free to adjust
  signatures if Phase 1's mapping makes something cleaner (e.g. dropping
  `find_ref`'s scan per the note above), but no reason expected to change
  their names or the domain concepts they express.
- Re-derive `Store<R, O>`/`RepoStore<'r>` as a thin wrapper holding a
  `gix_store::Store<R, O>` (or a `gix_store::Kind<Typed<Note>, R, O>`
  directly — decide once `Kind<'s, ...>`'s borrow of `&'s Store<R, O>` is
  worked out against `gix-anchor`'s existing "construct once, call many
  times" `RepoStore::open` usage).
- `Error`: reuse the existing transparent-boxing pattern (`Error::git`,
  already `Box<dyn std::error::Error + Send + Sync>`) for
  `gix_store::Error` — no new variant needed unless a specific
  `gix_store::Error` case (e.g. `NameTaken`, now unreachable in practice)
  deserves its own arm for a better message.
  `Error::GenesisExists` stays, now backed by `gix_store::Error::NameTaken`
  on a genuine full-oid collision.
- Update `crates/git-anchor` and `crates/gix-comment` in the same phase for whatever, if anything, changed in `Store`'s public shape.
  Expected to be nothing beyond internals, but this phase is where it gets fixed if something does.

## Phase 3 — Tests

- `FlakyRefStore`, `SplitIdentity`, `MemoryRefStore`-backed fixtures in `store.rs`'s test module are unaffected in kind: `gix_store::Store<R, O>` is generic over the same `R: RefStore + Committer, O: Find + Write` bounds `gix-anchor`'s local `Store` used, so every existing CAS-retry test (`attach_with_attachment_retries_and_forwards_the_winners_created_at`, `update_retries_and_carries_the_winners_binding_and_created_at_forward`, `remove_retries_and_deletes_the_winning_tip`, both `create_*` race tests) keeps exercising real retry logic — just inside `gix-store` now.
  Port them, don't rewrite their intent.
- Add a test asserting exactly one schema ref exists after anchoring notes
  against multiple distinct targets (regression guard for the single-global-
  kind decision).
- Add a test asserting both identity schemes' ref segments are full-oid
  (`refs/anchors/<target-hex>/<id-hex>`, 40 hex chars each) — regression
  guard against Phase 0 item 2 regressing to a truncated name.
- Port `gix-store`'s own `Kind`/`Put` tests for the new multi-segment
  `EntityName` and full-oid `anonymous()` behavior into `gix-store`'s test
  suite as part of Phase 0, not deferred to `gix-anchor`.
- `cargo test --workspace` passes, including `gix-comment` and
  `crates/git-anchor`'s integration tests, updated in Phase 2 if their call
  sites needed it.

## Phase 4 — Cleanup & docs

- `DEVPLAN.md`'s Phase 3 correction currently ends at "migrated onto `gix-refstore`."
  Update it to point here: the ref-CAS layer migrated onto `gix-refstore` first, then the whole entity engine (schema, commit-forward, history) migrated onto `gix-store` in this plan.
- `lib.rs`'s module doc and `store.rs`'s doc comment (currently describing
  a locally-owned "content-addressed store of notes") get updated to
  describe the `gix-store`-backed implementation.
- `crates/gix-anchor/README.md` and `crates/gix-comment/src/comment.rs`'s
  doc comment (name-drops `gix_anchor::Store`) — check both still read
  accurately after Phase 2.

## Open decisions

1. **`EntityName`'s exact shape** (Phase 0 item 1) — settle when writing
   the `gix-store` change: a dedicated struct vs. a type alias over
   `Vec<RefSegment>` vs. accepting `impl IntoIterator<Item = RefSegment>`
   directly at each call site with no named type at all.
2. **`Store<R, O>`'s internal shape** (Phase 2) — newtype around a
   `gix_store::Kind` directly, vs. holding a `gix_store::Store<R, O>` and
   building a `Kind` per call — depends on how `Kind<'s, ...>`'s lifetime
   plays against `RepoStore::open`'s "build once, call many times" shape.
3. **Whether `find_ref`'s linear scan-by-id can be dropped** once both
   identity schemes name entities by their full oid (Phase 1's last bullet)
   — likely yes for `create`/`update`/`get`/`remove` (id *is* the entity
   name, so no scan needed), but `attach`'s binding-keyed scheme still
   names by the *binding's* oid, not the note's own — confirm this doesn't
   quietly break `attach`-based lookups by note id before deleting the scan.

## Definition of done

- `gix-anchor/src/store.rs` contains no commit-writing, tree-building, or
  ref-history-walking code of its own — only `Note`'s shape, the entity
  name/kind mapping, and translation to/from `StoredNote`.
- `gix-store`'s `Kind` supports multi-segment entity names and full-oid
  `anonymous()` naming; its own test suite covers both.
- `cargo test --workspace` passes in both `git-store` and `git-anchor`.
- `gix-anchor/Cargo.toml` depends on `gix-store`; direct use of
  `facet_git_tree`'s serialize/deserialize for `Note` (as opposed to
  `Binding`, which keeps it) is gone.
- No fallback, shim, or compatibility layer exists anywhere in this plan —
  there was nothing to stay compatible with.
