//! The checkpoints tree as one pure function (design 5.11, spec 3.8): which
//! row follows which, how deep it is and how the branch lines up. The view
//! only turns these rows into `├─` / `└─` / `│` characters.

use std::collections::{HashMap, HashSet};

use hypervctl_core::{Checkpoint, CheckpointId};

/// One checkpoint in tree order, for drawing its branch (design 5.11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub id: CheckpointId,
    /// 0 for a root, one more per level of depth.
    pub depth: usize,
    /// Whether this row is the last among its siblings (draws `└─`).
    pub last_in_siblings: bool,
    /// For every ancestor above this row: whether that ancestor was the last
    /// of its own siblings. `true` ends the `│` continuation of that level.
    pub ancestors_last: Vec<bool>,
}

impl TreeRow {
    /// The branch characters in front of the name: nothing for a root, `│  ` /
    /// `   ` per ancestor, then `├─ ` or `└─ ` for the row itself (spec 3.8).
    pub fn prefix(&self) -> String {
        if self.depth == 0 {
            return String::new();
        }
        let mut prefix = String::new();
        for last in &self.ancestors_last {
            prefix.push_str(if *last { "   " } else { "│  " });
        }
        prefix.push_str(if self.last_in_siblings {
            "└─ "
        } else {
            "├─ "
        });
        prefix
    }
}

/// The checkpoints in display order: children directly after their parent,
/// siblings by creation time (spec 3.8).
///
/// A parent that is not in `items` makes its child a root, and a cycle in
/// `parent_id` cannot loop forever: visited rows are never entered twice
/// (design 5.11, anomaly 30).
pub fn tree_rows(items: &[Checkpoint]) -> Vec<TreeRow> {
    let ids: HashSet<CheckpointId> = items.iter().map(|entry| entry.id).collect();
    let mut children: HashMap<CheckpointId, Vec<&Checkpoint>> = HashMap::new();
    let mut roots: Vec<&Checkpoint> = Vec::new();
    for entry in items {
        match entry.parent_id {
            Some(parent) if parent != entry.id && ids.contains(&parent) => {
                children.entry(parent).or_default().push(entry);
            }
            // No parent, a parent that is missing, or a self-reference: a root.
            _ => roots.push(entry),
        }
    }
    for siblings in children.values_mut() {
        siblings.sort_by(|a, b| by_creation(a, b));
    }
    roots.sort_by(|a, b| by_creation(a, b));

    let mut visited: HashSet<CheckpointId> = HashSet::new();
    let mut rows: Vec<TreeRow> = Vec::new();
    for (index, root) in roots.iter().enumerate() {
        if !visited.insert(root.id) {
            continue;
        }
        let last = index + 1 == roots.len();
        rows.push(TreeRow {
            id: root.id,
            depth: 0,
            last_in_siblings: last,
            ancestors_last: Vec::new(),
        });
        push_children(
            root.id,
            1,
            &mut Vec::new(),
            &children,
            &mut visited,
            &mut rows,
        );
    }
    // Rows caught in a pure cycle never reach a root; they are listed as
    // extra roots so that no checkpoint disappears (anomaly 30).
    for entry in items {
        if visited.insert(entry.id) {
            rows.push(TreeRow {
                id: entry.id,
                depth: 0,
                last_in_siblings: true,
                ancestors_last: Vec::new(),
            });
        }
    }
    rows
}

/// The same order [`tree_rows`] uses, applied to the checkpoints themselves,
/// so the store can keep one canonical ordering (spec 3.8).
pub fn order(items: Vec<Checkpoint>) -> Vec<Checkpoint> {
    let rows = tree_rows(&items);
    let mut by_id: HashMap<CheckpointId, Checkpoint> =
        items.into_iter().map(|entry| (entry.id, entry)).collect();
    rows.into_iter()
        .filter_map(|row| by_id.remove(&row.id))
        .collect()
}

fn push_children(
    parent: CheckpointId,
    depth: usize,
    ancestors_last: &mut Vec<bool>,
    children: &HashMap<CheckpointId, Vec<&Checkpoint>>,
    visited: &mut HashSet<CheckpointId>,
    rows: &mut Vec<TreeRow>,
) {
    let empty = Vec::new();
    let siblings = children.get(&parent).unwrap_or(&empty);
    for (index, child) in siblings.iter().enumerate() {
        if !visited.insert(child.id) {
            continue;
        }
        let last = index + 1 == siblings.len();
        rows.push(TreeRow {
            id: child.id,
            depth,
            last_in_siblings: last,
            ancestors_last: ancestors_last.clone(),
        });
        ancestors_last.push(last);
        push_children(child.id, depth + 1, ancestors_last, children, visited, rows);
        ancestors_last.pop();
    }
}

/// Creation time ascending; rows without a timestamp sort first and equal
/// times fall back to the name so that the order is stable (spec 3.8).
fn by_creation(a: &Checkpoint, b: &Checkpoint) -> std::cmp::Ordering {
    a.created.cmp(&b.created).then_with(|| a.name.cmp(&b.name))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    fn checkpoint(id: u128, name: &str, created: u64, parent: Option<u128>) -> Checkpoint {
        Checkpoint {
            id: CheckpointId::new(uuid::Uuid::from_u128(id)),
            name: name.to_string(),
            kind: hypervctl_core::CheckpointKind::Standard,
            created: Some(UNIX_EPOCH + Duration::from_secs(created)),
            parent_id: parent.map(|id| CheckpointId::new(uuid::Uuid::from_u128(id))),
        }
    }

    fn ids(rows: &[TreeRow]) -> Vec<u128> {
        rows.iter()
            .map(|row| row.id.into_uuid().as_u128())
            .collect::<Vec<_>>()
    }

    #[test]
    fn children_follow_their_parent_and_siblings_by_creation() {
        // The input order is scrambled on purpose: the tree decides.
        let items = vec![
            checkpoint(3, "child-b", 30, Some(1)),
            checkpoint(1, "root", 10, None),
            checkpoint(4, "child-c", 40, Some(1)),
            checkpoint(2, "child-a", 20, Some(1)),
        ];
        let rows = tree_rows(&items);
        assert_eq!(ids(&rows), vec![1, 2, 3, 4]);
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].depth, 1);
        // The three children of root: only the last one is `└─` (spec 3.8).
        assert_eq!(
            rows[1..]
                .iter()
                .map(|row| row.last_in_siblings)
                .collect::<Vec<_>>(),
            vec![false, false, true]
        );
    }

    #[test]
    fn rows_without_a_parent_that_exists_become_roots() {
        // parent 99 is not in the list: the child must not panic or vanish.
        let items = vec![
            checkpoint(2, "orphan", 20, Some(99)),
            checkpoint(1, "root", 10, None),
        ];
        let rows = tree_rows(&items);
        // Roots are sorted by creation as well: root (t=10) first.
        assert_eq!(ids(&rows), vec![1, 2]);
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].depth, 0);
    }

    #[test]
    fn a_cycle_stops_and_keeps_every_row() {
        // a -> b -> a: neither is a root, both are still shown (anomaly 30),
        // each as an extra root after the rows that had a path from a root.
        let items = vec![
            checkpoint(1, "a", 10, Some(2)),
            checkpoint(2, "b", 20, Some(1)),
        ];
        let rows = tree_rows(&items);
        assert_eq!(ids(&rows), vec![1, 2]);
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].depth, 0);
        // A deeper cycle hangs after the cycle-free part of the list.
        let items = vec![
            checkpoint(1, "root", 10, None),
            checkpoint(2, "a", 20, Some(3)),
            checkpoint(3, "b", 30, Some(2)),
            checkpoint(4, "child", 40, Some(1)),
        ];
        let rows = tree_rows(&items);
        assert_eq!(ids(&rows), vec![1, 4, 2, 3]);
        assert_eq!(rows[0].depth, 0, "the root");
        assert_eq!(rows[1].depth, 1, "the root's child");
        assert_eq!(rows[2].depth, 0, "the cycle hangs off as extra roots");
        assert_eq!(rows[3].depth, 0);
    }

    #[test]
    fn order_moves_every_checkpoint_to_its_tree_position() {
        let items = vec![
            checkpoint(3, "grandchild", 30, Some(2)),
            checkpoint(2, "child", 20, Some(1)),
            checkpoint(1, "root", 10, None),
        ];
        let ordered = order(items);
        let names: Vec<&str> = ordered.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["root", "child", "grandchild"]);
    }

    #[test]
    fn rows_without_a_timestamp_sort_first() {
        let mut late = checkpoint(2, "late", 90, Some(1));
        late.created = None;
        let items = vec![
            checkpoint(1, "root", 10, None),
            late,
            checkpoint(3, "early", 20, Some(1)),
        ];
        let rows = tree_rows(&items);
        assert_eq!(ids(&rows), vec![1, 2, 3]);
    }

    #[test]
    fn a_self_parent_counts_as_a_root() {
        let items = vec![checkpoint(1, "itself", 10, Some(1))];
        let rows = tree_rows(&items);
        assert_eq!(ids(&rows), vec![1]);
        assert_eq!(rows[0].depth, 0);
    }

    /// The branch characters of spec 3.8, straight from the row.
    #[test]
    fn the_prefix_draws_the_branch_lines() {
        let items = vec![
            checkpoint(1, "root", 10, None),
            checkpoint(2, "child-a", 20, Some(1)),
            checkpoint(3, "child-b", 30, Some(1)),
            checkpoint(4, "grandchild", 40, Some(2)),
        ];
        let rows = tree_rows(&items);
        let prefixes: Vec<String> = rows.iter().map(TreeRow::prefix).collect();
        // Depth first: the grandchild of child-a comes before child-b.
        assert_eq!(prefixes, vec!["", "├─ ", "│  └─ ", "└─ "], "{prefixes:?}");
    }
}
