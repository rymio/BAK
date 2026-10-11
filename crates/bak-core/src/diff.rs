//! Snapshot diff, computed from two entry listings. The engine has no diff call.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::engine::Entry;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DiffReport {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
}

impl DiffReport {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// Paths added, removed and changed going from `a` to `b`. A directory counts
/// as changed only if its own metadata changed, not its contents.
pub fn diff(a: &[Entry], b: &[Entry]) -> DiffReport {
    let index = |entries: &[Entry]| -> BTreeMap<PathBuf, usize> {
        entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.raw_path.clone(), i))
            .collect()
    };
    let (ia, ib) = (index(a), index(b));
    let mut report = DiffReport::default();
    for (path, &i) in &ia {
        match ib.get(path) {
            None => report.removed.push(a[i].path.clone()),
            Some(&j) if differs(&a[i], &b[j]) => report.changed.push(b[j].path.clone()),
            Some(_) => {}
        }
    }
    for (path, &j) in &ib {
        if !ia.contains_key(path) {
            report.added.push(b[j].path.clone());
        }
    }
    report
}

fn differs(x: &Entry, y: &Entry) -> bool {
    x.kind != y.kind
        || x.mode != y.mode
        || x.link_target != y.link_target
        || (x.kind != crate::engine::EntryKind::Dir && (x.size != y.size || x.content != y.content))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::EntryKind;

    fn file(path: &str, content: &str) -> Entry {
        Entry {
            path: path.into(),
            raw_path: path.into(),
            kind: EntryKind::File,
            size: content.len() as u64,
            mode: Some(0o644),
            mtime: None,
            link_target: None,
            content: Some(content.into()),
        }
    }

    #[test]
    fn finds_added_removed_changed() {
        let a = vec![file("a", "1"), file("b", "2"), file("c", "3")];
        let b = vec![file("a", "1"), file("b", "X"), file("d", "4")];
        let d = diff(&a, &b);
        assert_eq!(d.added, vec!["d"]);
        assert_eq!(d.removed, vec!["c"]);
        assert_eq!(d.changed, vec!["b"]);
    }
}
