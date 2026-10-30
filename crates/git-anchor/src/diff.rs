//! Structural diff between two git trees, by object id.
//!
//! Spec coverage: `anchor.tree-pair-diff`.
//!
//! [`diff_trees`] walks two trees in lockstep using only [`gix_object::Find`]
//! — no `gix::Repository`, no working tree — and prunes any pair of entries
//! whose object id is equal on both sides, since content addressing already
//! guarantees an equal id means an equal (sub)tree. Cost is proportional to
//! the number of entries that actually changed, not to either tree's size.

use std::collections::HashMap;

use gix::ObjectId;
use gix_object::tree::EntryMode;
use gix_object::{Find, Kind};

use crate::error::Error;

/// One entry that differs between two trees, named by full slash-joined path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeChange {
    /// The full slash-joined path of the differing entry.
    pub path: String,
    /// The entry's object id and mode on the base side, or `None` when the
    /// entry was added on the head side.
    pub base: Option<(ObjectId, EntryMode)>,
    /// The entry's object id and mode on the head side, or `None` when the
    /// entry was deleted on the head side.
    pub head: Option<(ObjectId, EntryMode)>,
}

/// Structurally diff `base` against `head` (`anchor.tree-pair-diff`).
///
/// `None` on either side, or the canonical empty-tree id
/// (`4b825dc642cb6eb9a060e54bf8d69288fbee4904`), both mean "empty tree", so a
/// whole-tree addition and a whole-tree deletion are ordinary cases of the
/// same walk, not special-cased separately. The result is sorted by `path`;
/// an entry whose object id is equal on both sides is pruned rather than
/// descended into, so an unchanged subtree costs exactly one id comparison.
pub fn diff_trees<F>(
    base: Option<ObjectId>,
    head: Option<ObjectId>,
    store: &F,
) -> Result<Vec<TreeChange>, Error>
where
    F: Find + ?Sized,
{
    let mut changes = Vec::new();
    walk(String::new(), base, head, store, &mut changes)?;
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(changes)
}

/// Diff the tree pair at `base`/`head` (`None` meaning empty), appending
/// every changed entry under `prefix` to `out`.
///
/// Recurses only into a name present, with differing object ids, on both
/// sides where both entries are trees; every other case — an id shared by
/// both sides, a name unique to one side, or a differing pair where either
/// side is not a tree — is resolved without recursion, in this frame.
fn walk<F>(
    prefix: String,
    base: Option<ObjectId>,
    head: Option<ObjectId>,
    store: &F,
    out: &mut Vec<TreeChange>,
) -> Result<(), Error>
where
    F: Find + ?Sized,
{
    if base == head {
        return Ok(());
    }
    let base_entries = entries(base, store)?;
    let head_entries = entries(head, store)?;
    let mut remaining: HashMap<&str, (ObjectId, EntryMode)> = base_entries
        .iter()
        .map(|(name, id, mode)| (name.as_str(), (*id, *mode)))
        .collect();

    for (name, head_id, head_mode) in &head_entries {
        let path = join(&prefix, name);
        match remaining.remove(name.as_str()) {
            Some((base_id, _base_mode)) if base_id == *head_id => {}
            Some((base_id, base_mode)) if base_mode.is_tree() && head_mode.is_tree() => {
                walk(path, Some(base_id), Some(*head_id), store, out)?;
            }
            Some((base_id, base_mode)) => out.push(TreeChange {
                path,
                base: Some((base_id, base_mode)),
                head: Some((*head_id, *head_mode)),
            }),
            None if head_mode.is_tree() => walk(path, None, Some(*head_id), store, out)?,
            None => out.push(TreeChange {
                path,
                base: None,
                head: Some((*head_id, *head_mode)),
            }),
        }
    }

    for (name, (base_id, base_mode)) in remaining {
        let path = join(&prefix, name);
        if base_mode.is_tree() {
            walk(path, Some(base_id), None, store, out)?;
        } else {
            out.push(TreeChange {
                path,
                base: Some((base_id, base_mode)),
                head: None,
            });
        }
    }

    Ok(())
}

/// `prefix/name`, or bare `name` when `prefix` is empty (the root).
fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}/{name}")
    }
}

/// The decoded entries of the tree named by `id`, or none of them when `id`
/// is `None` or the canonical empty-tree id — both stand for a tree with no
/// entries, so every caller can treat a missing side uniformly.
fn entries<F>(id: Option<ObjectId>, store: &F) -> Result<Vec<(String, ObjectId, EntryMode)>, Error>
where
    F: Find + ?Sized,
{
    let Some(id) = id else {
        return Ok(Vec::new());
    };
    if id.is_empty_tree() {
        return Ok(Vec::new());
    }
    let mut buf = Vec::new();
    let data = store
        .try_find(&id, &mut buf)
        .map_err(|error| Error::Object(error.to_string()))?
        .ok_or_else(|| Error::Object(format!("object {id} not found")))?;
    if data.kind != Kind::Tree {
        return Err(Error::Object(format!("{id} is not a tree")));
    }
    let tree = gix_object::TreeRef::from_bytes(data.data, id.kind())
        .map_err(|error| Error::Object(error.to_string()))?;
    tree.entries
        .iter()
        .map(|entry| {
            let name = std::str::from_utf8(entry.filename)
                .map_err(|_error| Error::Object(format!("non-utf8 entry name in tree {id}")))?;
            Ok((name.to_owned(), entry.oid.to_owned(), entry.mode))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::arithmetic_side_effects,
        reason = "unit test"
    )]

    use std::cell::Cell;
    use std::collections::BTreeMap;

    use facet_git_tree::ObjectStore;
    use gix_object::{Data, Tree, Write as _, WriteTo, tree};
    use proptest::prelude::*;

    use super::*;

    const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    fn empty_tree_id() -> ObjectId {
        ObjectId::from_hex(EMPTY_TREE.as_bytes()).expect("valid hex")
    }

    fn write_blob(store: &ObjectStore, content: &[u8]) -> ObjectId {
        store.write_buf(Kind::Blob, content).expect("write blob")
    }

    /// Build a tree object from `entries` (already-known name/id/mode
    /// triples) and write it to `store`, returning its id.
    fn write_tree(store: &ObjectStore, entries: Vec<(&str, ObjectId, EntryMode)>) -> ObjectId {
        let mut tree = Tree {
            entries: entries
                .into_iter()
                .map(|(name, oid, mode)| tree::Entry {
                    mode,
                    filename: name.into(),
                    oid,
                })
                .collect(),
        };
        tree.entries.sort();
        store.write(&tree as &dyn WriteTo).expect("write tree")
    }

    /// Write a tree holding one blob per `path -> content` entry (all paths
    /// single-segment: these fixtures build one level at a time).
    fn write_flat_tree(store: &ObjectStore, files: &[(&str, &[u8])]) -> ObjectId {
        let entries = files
            .iter()
            .map(|(name, content)| {
                (
                    *name,
                    write_blob(store, content),
                    EntryMode::from(tree::EntryKind::Blob),
                )
            })
            .collect();
        write_tree(store, entries)
    }

    fn blob_mode() -> EntryMode {
        EntryMode::from(tree::EntryKind::Blob)
    }

    fn tree_mode() -> EntryMode {
        EntryMode::from(tree::EntryKind::Tree)
    }

    #[test]
    fn empty_base_and_empty_head_diff_to_nothing() {
        let store = ObjectStore::default();
        assert_eq!(diff_trees(None, None, &store).unwrap(), Vec::new());
        let empty = empty_tree_id();
        assert_eq!(
            diff_trees(Some(empty), Some(empty), &store).unwrap(),
            Vec::new()
        );
        assert_eq!(diff_trees(None, Some(empty), &store).unwrap(), Vec::new());
    }

    #[test]
    fn add_only_from_an_empty_base() {
        let store = ObjectStore::default();
        let head = write_flat_tree(&store, &[("a.txt", b"a"), ("b.txt", b"b")]);

        let changes = diff_trees(None, Some(head), &store).unwrap();
        assert_eq!(
            changes,
            vec![
                TreeChange {
                    path: "a.txt".to_owned(),
                    base: None,
                    head: Some((write_blob(&store, b"a"), blob_mode())),
                },
                TreeChange {
                    path: "b.txt".to_owned(),
                    base: None,
                    head: Some((write_blob(&store, b"b"), blob_mode())),
                },
            ]
        );
    }

    #[test]
    fn delete_only_to_an_empty_tip() {
        let store = ObjectStore::default();
        let base = write_flat_tree(&store, &[("a.txt", b"a"), ("b.txt", b"b")]);

        let changes = diff_trees(Some(base), None, &store).unwrap();
        assert_eq!(
            changes,
            vec![
                TreeChange {
                    path: "a.txt".to_owned(),
                    base: Some((write_blob(&store, b"a"), blob_mode())),
                    head: None,
                },
                TreeChange {
                    path: "b.txt".to_owned(),
                    base: Some((write_blob(&store, b"b"), blob_mode())),
                    head: None,
                },
            ]
        );
    }

    #[test]
    fn a_modified_blob_is_one_modification() {
        let store = ObjectStore::default();
        let base = write_flat_tree(&store, &[("a.txt", b"old"), ("b.txt", b"unchanged")]);
        let head = write_flat_tree(&store, &[("a.txt", b"new"), ("b.txt", b"unchanged")]);

        let changes = diff_trees(Some(base), Some(head), &store).unwrap();
        assert_eq!(
            changes,
            vec![TreeChange {
                path: "a.txt".to_owned(),
                base: Some((write_blob(&store, b"old"), blob_mode())),
                head: Some((write_blob(&store, b"new"), blob_mode())),
            }]
        );
    }

    #[test]
    fn an_added_nested_subtree_expands_to_its_leaves() {
        let store = ObjectStore::default();
        let base = write_flat_tree(&store, &[("root.txt", b"root")]);
        let nested = write_flat_tree(&store, &[("x.txt", b"x"), ("y.txt", b"y")]);
        let head = write_tree(
            &store,
            vec![
                ("root.txt", write_blob(&store, b"root"), blob_mode()),
                ("nested", nested, tree_mode()),
            ],
        );

        let changes = diff_trees(Some(base), Some(head), &store).unwrap();
        let paths: Vec<&str> = changes.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, vec!["nested/x.txt", "nested/y.txt"]);
        assert!(changes.iter().all(|c| c.base.is_none()));
    }

    #[test]
    fn an_unchanged_sibling_subtree_is_pruned_not_descended() {
        let store = ObjectStore::default();
        let unchanged = write_flat_tree(&store, &[("x.txt", b"x"), ("y.txt", b"y")]);
        let base = write_tree(
            &store,
            vec![
                ("changed.txt", write_blob(&store, b"old"), blob_mode()),
                ("same", unchanged, tree_mode()),
            ],
        );
        let head = write_tree(
            &store,
            vec![
                ("changed.txt", write_blob(&store, b"new"), blob_mode()),
                ("same", unchanged, tree_mode()),
            ],
        );

        let changes = diff_trees(Some(base), Some(head), &store).unwrap();
        assert_eq!(
            changes,
            vec![TreeChange {
                path: "changed.txt".to_owned(),
                base: Some((write_blob(&store, b"old"), blob_mode())),
                head: Some((write_blob(&store, b"new"), blob_mode())),
            }]
        );
    }

    #[test]
    fn a_rename_reports_as_a_delete_and_an_add() {
        let store = ObjectStore::default();
        let base = write_flat_tree(&store, &[("old_name.txt", b"same content")]);
        let head = write_flat_tree(&store, &[("new_name.txt", b"same content")]);

        let changes = diff_trees(Some(base), Some(head), &store).unwrap();
        let content_id = write_blob(&store, b"same content");
        assert_eq!(
            changes,
            vec![
                TreeChange {
                    path: "new_name.txt".to_owned(),
                    base: None,
                    head: Some((content_id, blob_mode())),
                },
                TreeChange {
                    path: "old_name.txt".to_owned(),
                    base: Some((content_id, blob_mode())),
                    head: None,
                },
            ]
        );
    }

    /// A [`gix_object::Find`] wrapper counting every `try_find` call, so a
    /// test can assert the walk reads O(depth) objects rather than O(tree
    /// size) for a single-leaf change buried in an otherwise identical tree.
    struct CountingStore<'a> {
        inner: &'a ObjectStore,
        calls: Cell<usize>,
    }

    impl Find for CountingStore<'_> {
        fn try_find<'b>(
            &self,
            id: &gix::hash::oid,
            buffer: &'b mut Vec<u8>,
        ) -> Result<Option<Data<'b>>, gix_object::find::Error> {
            self.calls.set(self.calls.get() + 1);
            self.inner.try_find(id, buffer)
        }
    }

    #[test]
    fn pruning_reads_proportionally_to_depth_not_tree_size() {
        let store = ObjectStore::default();

        // A wide, two-level tree: `width` unchanged sibling subtrees, each
        // holding `width` blobs, plus one subtree with a single changed leaf.
        let width = 30u32;
        let unchanged_files: Vec<(String, &[u8])> = (0..width)
            .map(|n| (format!("f{n}.txt"), b"x" as &[u8]))
            .collect();
        let unchanged_refs: Vec<(&str, &[u8])> = unchanged_files
            .iter()
            .map(|(n, c)| (n.as_str(), *c))
            .collect();
        let unchanged_sub = write_flat_tree(&store, &unchanged_refs);

        let mut changed_files: Vec<(String, &[u8])> = (0..width)
            .map(|n| (format!("f{n}.txt"), b"x" as &[u8]))
            .collect();
        if let Some(first) = changed_files.first_mut() {
            first.1 = b"changed";
        }
        let changed_refs: Vec<(&str, &[u8])> = changed_files
            .iter()
            .map(|(n, c)| (n.as_str(), *c))
            .collect();
        let base_changed_sub = write_flat_tree(&store, &unchanged_refs);
        let head_changed_sub = write_flat_tree(&store, &changed_refs);

        let base_top: Vec<(String, ObjectId, EntryMode)> = (0..width)
            .map(|group| {
                let sub = if group == 0 {
                    base_changed_sub
                } else {
                    unchanged_sub
                };
                (format!("group{group}"), sub, tree_mode())
            })
            .collect();
        let head_top: Vec<(String, ObjectId, EntryMode)> = (0..width)
            .map(|group| {
                let sub = if group == 0 {
                    head_changed_sub
                } else {
                    unchanged_sub
                };
                (format!("group{group}"), sub, tree_mode())
            })
            .collect();
        let base = write_tree(
            &store,
            base_top
                .iter()
                .map(|(n, id, m)| (n.as_str(), *id, *m))
                .collect(),
        );
        let head = write_tree(
            &store,
            head_top
                .iter()
                .map(|(n, id, m)| (n.as_str(), *id, *m))
                .collect(),
        );

        let counting = CountingStore {
            inner: &store,
            calls: Cell::new(0),
        };
        let changes = diff_trees(Some(base), Some(head), &counting).unwrap();
        assert_eq!(
            changes,
            vec![TreeChange {
                path: "group0/f0.txt".to_owned(),
                base: Some((write_blob(&store, b"x"), blob_mode())),
                head: Some((write_blob(&store, b"changed"), blob_mode())),
            }]
        );

        // Two tree reads (the two top-level trees) plus two more (the one
        // changed second-level tree, on each side): four, regardless of
        // `width`, since every unchanged sibling is pruned by its shared id
        // before ever being read.
        assert_eq!(counting.calls.get(), 4);
    }

    /// Build a tree from a flat `path -> bytes` map, one blob per entry (all
    /// single-segment names — enough to exercise the property below without
    /// nested trees, which the dedicated unit tests above already cover).
    fn build(store: &ObjectStore, files: &BTreeMap<String, Vec<u8>>) -> Option<ObjectId> {
        if files.is_empty() {
            return None;
        }
        let entries = files
            .iter()
            .map(|(name, content)| (name.as_str(), write_blob(store, content), blob_mode()))
            .collect();
        Some(write_tree(store, entries))
    }

    proptest! {
        /// `diff_trees` reports exactly the symmetric-difference/changed set
        /// a naive full comparison of two `path -> bytes` maps computes
        /// (`anchor.tree-pair-diff`).
        #[test]
        fn diff_trees_reports_every_changed_leaf(
            base_map in prop::collection::btree_map("[a-e]{1,3}", prop::collection::vec(any::<u8>(), 0..8), 0..6),
            head_map in prop::collection::btree_map("[a-e]{1,3}", prop::collection::vec(any::<u8>(), 0..8), 0..6),
        ) {
            let store = ObjectStore::default();
            let base = build(&store, &base_map);
            let head = build(&store, &head_map);

            let mut expected: Vec<String> = base_map.keys().chain(head_map.keys()).cloned().collect();
            expected.sort();
            expected.dedup();
            let expected: Vec<String> = expected
                .into_iter()
                .filter(|name| base_map.get(name) != head_map.get(name))
                .collect();

            let changes = diff_trees(base, head, &store).unwrap();
            let actual: Vec<String> = changes.iter().map(|c| c.path.clone()).collect();
            prop_assert_eq!(actual, expected);

            for change in &changes {
                prop_assert_eq!(change.base.is_some(), base_map.contains_key(&change.path));
                prop_assert_eq!(change.head.is_some(), head_map.contains_key(&change.path));
            }
        }
    }
}
