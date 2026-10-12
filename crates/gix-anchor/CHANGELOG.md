# Changelog

## 0.1.0 (2026-10-12)


### ⚠ BREAKING CHANGES

* reshape identity to byte spans and hints to fingerprints/descriptors
* store notes as gix-store entities instead of a local engine
* unify Anchor's identity fields on the shared Oid type
* derive Facet on Binding instead of hand-rolled untagged codec
* `Store` is generic; callers naming the type want `RepoStore<'r>`. `Error::CasExhausted` is replaced by
* `Store::with_prefix` takes `RefPrefix`, not `&'static str`, and `Error::InvalidRefComponent` is gone — the types make it unreachable.

### Features

* Add comment threads and resolvable state ([c424085](https://github.com/git-ents/git-anchor/commit/c4240856c34d88901b2807abd1764b9a26f61223))
* Add gix-anchor library and note store ([afd3b9d](https://github.com/git-ents/git-anchor/commit/afd3b9dd6747a762afa9e00e2e3b4782646fd47c))
* Add optional reachable note attachment to the store ([b962144](https://github.com/git-ents/git-anchor/commit/b96214441dee178f7a1f7c7be619706333973f45))
* Add position/content axes and batched projection to gix-anchor ([96bd5ba](https://github.com/git-ents/git-anchor/commit/96bd5ba6048185ef64fbbce995f89f11e183c201))
* Add Position/Content axis decomposition via Projection::axes ([96bd5ba](https://github.com/git-ents/git-anchor/commit/96bd5ba6048185ef64fbbce995f89f11e183c201))
* Add project_candidates for multi-path content survival ([96bd5ba](https://github.com/git-ents/git-anchor/commit/96bd5ba6048185ef64fbbce995f89f11e183c201))
* Add project_many for single-resolution batched projection ([96bd5ba](https://github.com/git-ents/git-anchor/commit/96bd5ba6048185ef64fbbce995f89f11e183c201))
* Add PROJECTION_HEURISTIC_VERSION for downstream cache keys ([96bd5ba](https://github.com/git-ents/git-anchor/commit/96bd5ba6048185ef64fbbce995f89f11e183c201))
* Expand git anchor CLI per UX review ([3b83aca](https://github.com/git-ents/git-anchor/commit/3b83aca9e84f7817111cf606940ecdcfc9d27308))
* Export REBIND_PIN_KIND alongside the schema it names ([c133e51](https://github.com/git-ents/git-anchor/commit/c133e513f5ad8f4dbcd99382d59c0fbe0753c368))
* Register the rebind-pin payload schema (A5) ([8d7cc5b](https://github.com/git-ents/git-anchor/commit/8d7cc5b0bdf8ccf7eb627dc7fee61de158acdc50))
* Reshape identity to byte spans and hints to fingerprints/descriptors ([ad8ef55](https://github.com/git-ents/git-anchor/commit/ad8ef55e6279052a297d5e805701d700d958a894))


### Bug Fixes

* Attribute a note to its author, not its committer ([2e7c52d](https://github.com/git-ents/git-anchor/commit/2e7c52dcca97119f33ee3c6f26bb66f3347d4ac8))
* Group injected entities by anchor id, not a hint-derived target ([a175ba4](https://github.com/git-ents/git-anchor/commit/a175ba45cdaadd784fa176fe205af1a069c21df1))
* Improve short-range fingerprint recovery ([fbb5ef8](https://github.com/git-ents/git-anchor/commit/fbb5ef8feb2fbad75c07629d2d0e7013cf44bee8))
* Order comment replies by a nanosecond creation tiebreaker ([9668673](https://github.com/git-ents/git-anchor/commit/96686730cf8784ca1550f0eb1b61e76e77027eac))
* Separate the capture handle from the hint-invariant anchor id ([086f595](https://github.com/git-ents/git-anchor/commit/086f595f4d5456e850f2d58f3b2dde4b2a876da9))
* Sort thread replies by created_at instead of a content-hash id ([9668673](https://github.com/git-ents/git-anchor/commit/96686730cf8784ca1550f0eb1b61e76e77027eac))
* Stamp every note with a nanosecond created_at and forward it across edits ([9668673](https://github.com/git-ents/git-anchor/commit/96686730cf8784ca1550f0eb1b61e76e77027eac))


### Documentation

* Bring READMEs and DEVPLAN-boundary.md in line with the boundary refactor ([c0a746c](https://github.com/git-ents/git-anchor/commit/c0a746cd1895e6298b00c3e09f98f06b8a77b5ac))
* Rewrite architecture and specs for the identity/hints split ([a9fe212](https://github.com/git-ents/git-anchor/commit/a9fe212844b13df65384f39ee90ea38a773d1b43))
* State crate boundaries and reconcile the storage plan with what shipped ([992387f](https://github.com/git-ents/git-anchor/commit/992387f5b2b1dc79034207d013670e76f9830ca7))


### Code Refactoring

* Derive Facet on Binding instead of hand-rolled untagged codec ([f96a3c4](https://github.com/git-ents/git-anchor/commit/f96a3c42949883321b0714a9810847bcf1d0a397))
* Embed Binding inline in the note document ([f18e04e](https://github.com/git-ents/git-anchor/commit/f18e04e57cd0710b1d7c31a5b4713216b1187a3e))
* Move the note document from gix-anchor into gix-comment ([2bb8f2f](https://github.com/git-ents/git-anchor/commit/2bb8f2fdc7c30548c7f755411d15f6e9df7835e0))
* Read a note ref's parts as segments, not substrings ([f00c0b3](https://github.com/git-ents/git-anchor/commit/f00c0b3b53696fbee7717c80e23603c444a5f2c8))
* Split Binding into identity and hints subtrees ([7c7a0f7](https://github.com/git-ents/git-anchor/commit/7c7a0f73c97e1f71d32785a22ce6787b9604ae3b))
* Store notes as gix-store entities instead of a local engine ([83f2874](https://github.com/git-ents/git-anchor/commit/83f2874d8ddc4a1489d796cd72169e74d7f7afb0))
* Store notes through gix-refstore's CAS primitive ([3d83edd](https://github.com/git-ents/git-anchor/commit/3d83eddcb8541edc8042a3f633aff4bbbbdf8c5d))
* Type the note-ref layout with gix-refstore names ([923fd1c](https://github.com/git-ents/git-anchor/commit/923fd1c51c0ced55d38914c2b39fa243cba96734))
* Unify Anchor's identity fields on the shared Oid type ([cd147ef](https://github.com/git-ents/git-anchor/commit/cd147ef928c1cef3770dd642619776acb8a8353e))


### Tests

* Align retention and docs with newline leaf encoding ([302cf74](https://github.com/git-ents/git-anchor/commit/302cf742e60bd73798fff4dd2ead874dc421529a))
* Cover the CAS paths the migration rewrote ([d338351](https://github.com/git-ents/git-anchor/commit/d338351a2ddc01bb1e4529a7f94d92cda68b13a6))
* Update binding roundtrip fixture for newline leaf encoding ([2097efd](https://github.com/git-ents/git-anchor/commit/2097efdaf6b3e73f60e9e033dc16d99eaeaf6a8a))
