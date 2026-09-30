//! Auto-select rules (FR-DUP-17, AC-09).

use std::cmp::Reverse;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{DupGroup, GroupKind};

/// A rule choosing the single file to keep in a group; every other member
/// is marked for deletion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoSelect {
    KeepOldest,
    KeepNewest,
    KeepShortestPath,
    KeepInFolder(PathBuf),
    KeepHighestResolution,
}

impl AutoSelect {
    pub fn label(&self) -> &str {
        match self {
            AutoSelect::KeepOldest => "Keep oldest",
            AutoSelect::KeepNewest => "Keep newest",
            AutoSelect::KeepShortestPath => "Keep shortest path",
            AutoSelect::KeepInFolder(_) => "Keep file in folder",
            AutoSelect::KeepHighestResolution => "Keep highest resolution",
        }
    }

    /// Plain-language description for the UI (NFR-USE-02).
    pub fn explanation(&self) -> &'static str {
        match self {
            AutoSelect::KeepOldest => {
                "Keeps the copy with the oldest modification date and marks the others for removal."
            }
            AutoSelect::KeepNewest => {
                "Keeps the most recently modified copy and marks the others for removal."
            }
            AutoSelect::KeepShortestPath => {
                "Keeps the copy with the shortest path (usually the 'original' location) and marks the others."
            }
            AutoSelect::KeepInFolder(_) => {
                "Keeps one copy located inside the preferred folder. Groups with no copy in that folder are left untouched."
            }
            AutoSelect::KeepHighestResolution => {
                "Keeps the copy with the most pixels. Similar-image groups are never auto-selected; review them manually."
            }
        }
    }
}

/// Apply `rule` to `group`. Returns one flag per file, `true` = mark for
/// deletion. At most `n − 1` files are ever marked (AC-09): exactly one is
/// kept when the rule applies, and nothing is marked when it does not
/// (groups under two files, `KeepInFolder` with no file in the folder).
/// Similar groups always return all `false` (FR-DUP-14); ties keep the
/// earliest file in group order.
pub fn auto_select(group: &DupGroup, rule: &AutoSelect) -> Vec<bool> {
    let n = group.files.len();
    let mut marks = vec![false; n];
    if n < 2 || matches!(group.kind, GroupKind::Similar { .. }) {
        return marks;
    }
    let files = group.files.iter().enumerate();
    let path_key = |p: &std::path::Path| (p.as_os_str().len(), p.to_path_buf());
    let keep = match rule {
        AutoSelect::KeepOldest => files.min_by_key(|(i, f)| (f.mtime, *i)).map(|(i, _)| i),
        AutoSelect::KeepNewest => files
            .min_by_key(|(i, f)| (Reverse(f.mtime), *i))
            .map(|(i, _)| i),
        AutoSelect::KeepShortestPath => files
            .min_by_key(|(i, f)| (path_key(&f.path), *i))
            .map(|(i, _)| i),
        AutoSelect::KeepInFolder(dir) => files
            .filter(|(_, f)| f.path.starts_with(dir))
            .min_by_key(|(i, f)| (path_key(&f.path), *i))
            .map(|(i, _)| i),
        AutoSelect::KeepHighestResolution => files
            .min_by_key(|(i, _)| {
                let px = group
                    .image_dims
                    .get(*i)
                    .copied()
                    .flatten()
                    .map_or(0, |(w, h)| u64::from(w) * u64::from(h));
                (Reverse(px), *i)
            })
            .map(|(i, _)| i),
    };
    if let Some(keep) = keep {
        for (i, m) in marks.iter_mut().enumerate() {
            *m = i != keep;
        }
    }
    marks
}

/// A selection is valid when at least one member stays unmarked. The UI
/// uses this to refuse marking every file of a group.
pub fn selection_is_valid(marked: &[bool]) -> bool {
    marked.iter().any(|m| !m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FileEntry;

    /// xorshift64* — deterministic, dependency-free.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    fn random_group(rng: &mut Rng) -> DupGroup {
        let n = rng.below(8) as usize; // includes 0 and 1
        let dirs = ["/a", "/a/b", "/c", "/pref", "/pref/sub"];
        let files: Vec<FileEntry> = (0..n)
            .map(|i| FileEntry {
                path: format!(
                    "{}/f{}{}",
                    dirs[rng.below(5) as usize],
                    i,
                    "x".repeat(rng.below(4) as usize)
                )
                .into(),
                size: 100,
                mtime: rng.below(4) as i64, // many ties
                file_id: None,
                device: None,
            })
            .collect();
        let image_dims = if rng.below(2) == 0 {
            Vec::new()
        } else {
            (0..n)
                .map(|_| (rng.below(3) != 0).then(|| (rng.below(3) as u32 * 100, 50)))
                .collect()
        };
        let kind = if rng.below(4) == 0 {
            GroupKind::Similar { max_distance: 3 }
        } else {
            GroupKind::Identical
        };
        DupGroup {
            kind,
            hash: None,
            size: 100,
            files,
            image_dims,
        }
    }

    #[test]
    fn never_marks_every_member_ac09() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let rules = [
            AutoSelect::KeepOldest,
            AutoSelect::KeepNewest,
            AutoSelect::KeepShortestPath,
            AutoSelect::KeepInFolder("/pref".into()),
            AutoSelect::KeepInFolder("/nowhere".into()),
            AutoSelect::KeepHighestResolution,
        ];
        for _ in 0..5000 {
            let g = random_group(&mut rng);
            for rule in &rules {
                let marks = auto_select(&g, rule);
                assert_eq!(marks.len(), g.files.len());
                let kept = marks.iter().filter(|m| !**m).count();
                if g.files.is_empty() {
                    continue;
                }
                assert!(selection_is_valid(&marks), "{rule:?} marked all of {g:?}");
                if matches!(g.kind, GroupKind::Similar { .. }) || g.files.len() < 2 {
                    assert_eq!(kept, g.files.len(), "similar/singleton groups untouched");
                } else if !matches!(rule, AutoSelect::KeepInFolder(_)) {
                    assert_eq!(kept, 1, "{rule:?} must keep exactly one");
                }
            }
        }
    }

    #[test]
    fn rules_pick_expected_file() {
        let f = |p: &str, mtime| FileEntry {
            path: p.into(),
            size: 1,
            mtime,
            file_id: None,
            device: None,
        };
        let g = DupGroup {
            kind: GroupKind::Identical,
            hash: None,
            size: 1,
            files: vec![f("/x/long/name", 5), f("/y/a", 1), f("/pref/zz/b", 9)],
            image_dims: vec![Some((10, 10)), None, Some((20, 20))],
        };
        assert_eq!(
            auto_select(&g, &AutoSelect::KeepOldest),
            [true, false, true]
        );
        assert_eq!(
            auto_select(&g, &AutoSelect::KeepNewest),
            [true, true, false]
        );
        assert_eq!(
            auto_select(&g, &AutoSelect::KeepShortestPath),
            [true, false, true]
        );
        assert_eq!(
            auto_select(&g, &AutoSelect::KeepInFolder("/pref".into())),
            [true, true, false]
        );
        assert_eq!(
            auto_select(&g, &AutoSelect::KeepInFolder("/none".into())),
            [false; 3]
        );
        assert_eq!(
            auto_select(&g, &AutoSelect::KeepHighestResolution),
            [true, true, false]
        );
        let similar = DupGroup {
            kind: GroupKind::Similar { max_distance: 2 },
            ..g
        };
        assert_eq!(auto_select(&similar, &AutoSelect::KeepOldest), [false; 3]);
        assert!(!selection_is_valid(&[true, true]));
        assert!(selection_is_valid(&[true, false]));
        assert!(!selection_is_valid(&[]));
        assert!(!AutoSelect::KeepOldest.explanation().is_empty());
    }
}
