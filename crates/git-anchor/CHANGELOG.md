# Changelog

## 0.1.0 (2026-10-12)


### ⚠ BREAKING CHANGES

* reshape identity to byte spans and hints to fingerprints/descriptors
* unify Anchor's identity fields on the shared Oid type
* derive Facet on Binding instead of hand-rolled untagged codec
* `Store` is generic; callers naming the type want `RepoStore<'r>`. `Error::CasExhausted` is replaced by

### Features

* Add comment threads and resolvable state ([c424085](https://github.com/git-ents/git-anchor/commit/c4240856c34d88901b2807abd1764b9a26f61223))
* Add git-anchor CLI ([2fe763f](https://github.com/git-ents/git-anchor/commit/2fe763f9b917e9014e51d0b46557bfe21af10c90))
* Expand git anchor CLI per UX review ([3b83aca](https://github.com/git-ents/git-anchor/commit/3b83aca9e84f7817111cf606940ecdcfc9d27308))
* Reshape identity to byte spans and hints to fingerprints/descriptors ([ad8ef55](https://github.com/git-ents/git-anchor/commit/ad8ef55e6279052a297d5e805701d700d958a894))
* Split git-anchor's add into create/inject over gix-store's DocumentBuilder ([abec59e](https://github.com/git-ents/git-anchor/commit/abec59eb16225058aa4a6562b523c38217557004))


### Bug Fixes

* Consume git-store document semantics ([884d6b0](https://github.com/git-ents/git-anchor/commit/884d6b02a3e03d3ec60f2b30687f70504bdaea56))
* Group injected entities by anchor id, not a hint-derived target ([a175ba4](https://github.com/git-ents/git-anchor/commit/a175ba45cdaadd784fa176fe205af1a069c21df1))
* Join the shared store layout and make injected anchors resolvable ([3c6d62b](https://github.com/git-ents/git-anchor/commit/3c6d62b1bf8478cad1005ba121d80e22df0f9509))
* Separate the capture handle from the hint-invariant anchor id ([086f595](https://github.com/git-ents/git-anchor/commit/086f595f4d5456e850f2d58f3b2dde4b2a876da9))


### Documentation

* Bring READMEs and DEVPLAN-boundary.md in line with the boundary refactor ([c0a746c](https://github.com/git-ents/git-anchor/commit/c0a746cd1895e6298b00c3e09f98f06b8a77b5ac))
* Rewrite architecture and specs for the identity/hints split ([a9fe212](https://github.com/git-ents/git-anchor/commit/a9fe212844b13df65384f39ee90ea38a773d1b43))
* State crate boundaries and reconcile the storage plan with what shipped ([992387f](https://github.com/git-ents/git-anchor/commit/992387f5b2b1dc79034207d013670e76f9830ca7))


### Code Refactoring

* Derive Facet on Binding instead of hand-rolled untagged codec ([f96a3c4](https://github.com/git-ents/git-anchor/commit/f96a3c42949883321b0714a9810847bcf1d0a397))
* Make git-anchor generic over registered schemas ([4014fff](https://github.com/git-ents/git-anchor/commit/4014fff95bcf943a3566114dcb9365c9810ad754))
* Move the note document from gix-anchor into gix-comment ([2bb8f2f](https://github.com/git-ents/git-anchor/commit/2bb8f2fdc7c30548c7f755411d15f6e9df7835e0))
* Split Binding into identity and hints subtrees ([7c7a0f7](https://github.com/git-ents/git-anchor/commit/7c7a0f73c97e1f71d32785a22ce6787b9604ae3b))
* Store notes through gix-refstore's CAS primitive ([3d83edd](https://github.com/git-ents/git-anchor/commit/3d83eddcb8541edc8042a3f633aff4bbbbdf8c5d))
* Unify Anchor's identity fields on the shared Oid type ([cd147ef](https://github.com/git-ents/git-anchor/commit/cd147ef928c1cef3770dd642619776acb8a8353e))
