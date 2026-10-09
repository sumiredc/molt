//! Which lines of the new content are changed from the old (PRD §4, §8.1):
//! only those are checked, so that code the user already had does not stop
//! unrelated edits.
//!
//! Lines are matched by the shortest edit script (Myers). A line of the new
//! content that the script keeps from the old is unchanged; every other line
//! is changed.

/// The lines of the new content that are changed from the old.
#[derive(Debug)]
pub struct ChangedLines {
    /// Indexed by 0-based line of the new content.
    changed: Vec<bool>,
}

impl ChangedLines {
    /// Whether the 1-based `line` of the new content is changed. A line
    /// outside the content counts as changed, so that a finding is never
    /// dropped for lack of a line to match it to.
    pub fn contains(&self, line: usize) -> bool {
        line.checked_sub(1)
            .and_then(|index| self.changed.get(index))
            .copied()
            .unwrap_or(true)
    }
}

/// The lines of `new` that are changed from `old`. When `old` is empty, every
/// line is.
pub fn changed_lines(old: &str, new: &str) -> ChangedLines {
    changed_lines_within(old, new, MAX_EDITS)
}

/// Beyond this many inserted and deleted lines, matching stops and every line
/// between the common head and tail counts as changed. Bounds the memory of
/// the edit script, which grows with the square of the edits.
const MAX_EDITS: usize = 1000;

fn changed_lines_within(old: &str, new: &str, max_edits: usize) -> ChangedLines {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let head = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let tail = old[head..]
        .iter()
        .rev()
        .zip(new[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old_middle = &old[head..old.len() - tail];
    let new_middle = &new[head..new.len() - tail];

    let mut changed = vec![false; new.len()];
    let middle = &mut changed[head..new.len() - tail];
    match kept_lines(old_middle, new_middle, max_edits) {
        Some(kept) => {
            for (line, kept) in middle.iter_mut().zip(kept) {
                *line = !kept;
            }
        }
        None => middle.fill(true),
    }
    ChangedLines { changed }
}

/// One line of [`line_diff`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLine<'a> {
    /// In both contents.
    Same(&'a str),
    /// Only in the old content.
    Removed(&'a str),
    /// Only in the new content.
    Added(&'a str),
}

/// The lines of `old` and `new` in order, each marked as kept, removed or added, by the same
/// edit script as [`changed_lines`]. Between two kept lines, the removed lines come before the
/// added ones.
pub fn line_diff<'a>(old: &'a str, new: &'a str) -> Vec<DiffLine<'a>> {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let head = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let tail = old[head..]
        .iter()
        .rev()
        .zip(new[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old_middle = &old[head..old.len() - tail];
    let new_middle = &new[head..new.len() - tail];

    let mut lines: Vec<DiffLine> = old[..head].iter().map(|l| DiffLine::Same(l)).collect();
    let kept = kept_lines(old_middle, new_middle, MAX_EDITS)
        .unwrap_or_else(|| vec![false; new_middle.len()]);
    // The kept lines of `new` are a subsequence of `old`, so matching each to the first equal
    // line of `old` after the previous match always finds one.
    let mut rest = old_middle.iter();
    let mut added: Vec<DiffLine> = Vec::new();
    for (line, kept) in new_middle.iter().zip(kept) {
        if !kept {
            added.push(DiffLine::Added(line));
            continue;
        }
        for removed in rest.by_ref() {
            if removed == line {
                break;
            }
            lines.push(DiffLine::Removed(removed));
        }
        lines.append(&mut added);
        lines.push(DiffLine::Same(line));
    }
    lines.extend(rest.map(|l| DiffLine::Removed(l)));
    lines.append(&mut added);
    lines.extend(old[old.len() - tail..].iter().map(|l| DiffLine::Same(l)));
    lines
}

/// For each line of `new`, whether the shortest edit script from `old` keeps
/// it. `None` when the script needs more than `max_edits` edits.
///
/// Myers' greedy algorithm: for each number of edits `d`, the furthest point
/// reached on each diagonal `k = x - y` (`x` in `old`, `y` in `new`), then
/// back from the end through the recorded points.
fn kept_lines(old: &[&str], new: &[&str], max_edits: usize) -> Option<Vec<bool>> {
    let (n, m) = (old.len(), new.len());
    let limit = max_edits.min(n + m);
    // `furthest[k + offset]`: the furthest `x` on diagonal `k`.
    let offset = limit + 1;
    let mut furthest = vec![0usize; 2 * limit + 3];
    // `trace[d]`: `furthest` before step `d`, for diagonals `-d..=d`.
    let mut trace: Vec<Vec<usize>> = Vec::new();
    for d in 0..=limit {
        trace.push(furthest[offset - d..=offset + d].to_vec());
        for k in (0..=2 * d).step_by(2).map(|i| i as isize - d as isize) {
            let at = |k: isize| (k + offset as isize) as usize;
            let down = k == -(d as isize)
                || (k != d as isize && furthest[at(k - 1)] < furthest[at(k + 1)]);
            let mut x = if down {
                furthest[at(k + 1)]
            } else {
                furthest[at(k - 1)] + 1
            };
            let mut y = (x as isize - k) as usize;
            while x < n && y < m && old[x] == new[y] {
                x += 1;
                y += 1;
            }
            furthest[at(k)] = x;
            if x >= n && y >= m {
                return Some(backtrack(&trace, n, m));
            }
        }
    }
    None
}

/// Walks back from `(n, m)` through the furthest points recorded in `trace`,
/// marking the lines of `new` on the diagonal runs as kept.
fn backtrack(trace: &[Vec<usize>], n: usize, m: usize) -> Vec<bool> {
    let mut kept = vec![false; m];
    let (mut x, mut y) = (n, m);
    for d in (0..trace.len()).rev() {
        let (prev_x, prev_y) = if d == 0 {
            (0, 0)
        } else {
            let before = &trace[d];
            let d = d as isize;
            let at = |k: isize| (k + d) as usize;
            let k = x as isize - y as isize;
            let prev_k = if k == -d || (k != d && before[at(k - 1)] < before[at(k + 1)]) {
                k + 1
            } else {
                k - 1
            };
            let prev_x = before[at(prev_k)];
            (prev_x, (prev_x as isize - prev_k) as usize)
        };
        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            kept[y] = true;
        }
        (x, y) = (prev_x, prev_y);
    }
    kept
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::*;
    use crate::checker::{CheckContext, CheckReport, check_file};
    use crate::profile::element::Element;
    use crate::profile::status::ItemKey;
    use crate::profile::store::Profile;

    fn changed(old: &str, new: &str) -> Vec<usize> {
        let lines = changed_lines(old, new);
        (1..=new.lines().count())
            .filter(|&line| lines.contains(line))
            .collect()
    }

    fn numbered(count: usize) -> Vec<String> {
        (1..=count).map(|n| format!("line {n}")).collect()
    }

    fn check(old: &str, new: &str) -> CheckReport {
        let profile = Profile::new();
        let own_modules = BTreeSet::new();
        let ctx = CheckContext {
            profile: &profile,
            own_modules: &own_modules,
        };
        check_file(Path::new("src/lib.rs"), old, new, &ctx)
    }

    fn reported(report: &CheckReport) -> Vec<(usize, ItemKey)> {
        match report {
            CheckReport::OutOfProfile(findings) => {
                findings.iter().map(|f| (f.line, f.key.clone())).collect()
            }
            other => panic!("expected OutOfProfile, got {other:?}"),
        }
    }

    #[test]
    fn when_old_is_empty_every_line_is_changed() {
        assert_eq!(changed("", "a\nb\nc\n"), [1, 2, 3]);
    }

    #[test]
    fn when_old_and_new_are_the_same_no_line_is_changed() {
        assert_eq!(changed("a\nb\nc\n", "a\nb\nc\n"), [] as [usize; 0]);
    }

    #[test]
    fn an_inserted_line_is_changed_and_the_lines_around_it_are_not() {
        assert_eq!(changed("a\nb\nc\n", "a\nb\nx\nc\n"), [3]);
    }

    #[test]
    fn a_rewritten_line_is_changed() {
        assert_eq!(changed("a\nb\nc\n", "a\nx\nc\n"), [2]);
    }

    #[test]
    fn deleting_a_line_changes_no_line_of_the_new_content() {
        assert_eq!(changed("a\nb\nc\n", "a\nc\n"), [] as [usize; 0]);
    }

    #[test]
    fn a_line_whose_indentation_changed_is_changed() {
        assert_eq!(changed("a\n    b\n", "a\n        b\n"), [2]);
    }

    #[test]
    fn edits_far_apart_change_only_their_own_lines() {
        let old = numbered(3000);
        let mut new = old.clone();
        new.insert(2000, "inserted".to_owned());
        new[10] = "rewritten".to_owned();
        new.remove(500);
        assert_eq!(changed(&old.join("\n"), &new.join("\n")), [11, 2000]);
    }

    #[test]
    fn a_line_repeated_in_new_is_changed_once_more_than_in_old() {
        let lines = changed("a\nx\nb\n", "a\nx\nb\nx\n");
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn beyond_the_edit_limit_every_line_between_the_common_head_and_tail_is_changed() {
        let lines = changed_lines_within("a\nb\nc\nd\ne\n", "a\nx\nc\ny\ne\n", 3);
        let changed: Vec<usize> = (1..=5).filter(|&l| lines.contains(l)).collect();
        assert_eq!(changed, [2, 3, 4]);
    }

    #[test]
    fn within_the_edit_limit_lines_are_matched() {
        let lines = changed_lines_within("a\nb\nc\nd\ne\n", "a\nx\nc\ny\ne\n", 4);
        let changed: Vec<usize> = (1..=5).filter(|&l| lines.contains(l)).collect();
        assert_eq!(changed, [2, 4]);
    }

    /// The length of the longest common subsequence, by the textbook table.
    fn lcs_len(old: &[&str], new: &[&str]) -> usize {
        let mut table = vec![vec![0usize; new.len() + 1]; old.len() + 1];
        for i in 0..old.len() {
            for j in 0..new.len() {
                table[i + 1][j + 1] = if old[i] == new[j] {
                    table[i][j] + 1
                } else {
                    table[i][j + 1].max(table[i + 1][j])
                };
            }
        }
        table[old.len()][new.len()]
    }

    #[test]
    fn the_kept_lines_are_a_longest_common_subsequence_of_old_and_new() {
        // A fixed linear congruential sequence: many small cases over a
        // 3-letter alphabet, so that lines repeat and match in many ways.
        let mut seed: u64 = 1;
        let mut next = move |bound: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            (seed >> 33) % bound
        };
        let letters = ["a", "b", "c"];
        for _ in 0..500 {
            let old: Vec<&str> = (0..next(9)).map(|_| letters[next(3) as usize]).collect();
            let new: Vec<&str> = (0..next(9)).map(|_| letters[next(3) as usize]).collect();
            let kept = kept_lines(&old, &new, usize::MAX).unwrap();
            let kept_new: Vec<&str> = new
                .iter()
                .zip(&kept)
                .filter(|(_, kept)| **kept)
                .map(|(line, _)| *line)
                .collect();
            assert_eq!(kept_new.len(), lcs_len(&old, &new), "{old:?} -> {new:?}");
            let mut rest = old.iter();
            assert!(
                kept_new.iter().all(|line| rest.any(|o| o == line)),
                "kept lines are not a subsequence of old: {old:?} -> {new:?}"
            );
        }
    }

    #[test]
    fn line_diff_gives_back_both_contents_and_keeps_a_longest_common_subsequence() {
        let mut seed: u64 = 7;
        let mut next = move |bound: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            (seed >> 33) % bound
        };
        let letters = ["a", "b", "c"];
        for _ in 0..500 {
            let old: Vec<&str> = (0..next(9)).map(|_| letters[next(3) as usize]).collect();
            let new: Vec<&str> = (0..next(9)).map(|_| letters[next(3) as usize]).collect();
            let (old_text, new_text) = (old.join("\n"), new.join("\n"));
            let diff = line_diff(&old_text, &new_text);
            let from_old: Vec<&str> = diff
                .iter()
                .filter_map(|l| match *l {
                    DiffLine::Same(s) | DiffLine::Removed(s) => Some(s),
                    DiffLine::Added(_) => None,
                })
                .collect();
            let from_new: Vec<&str> = diff
                .iter()
                .filter_map(|l| match *l {
                    DiffLine::Same(s) | DiffLine::Added(s) => Some(s),
                    DiffLine::Removed(_) => None,
                })
                .collect();
            let same = diff
                .iter()
                .filter(|l| matches!(l, DiffLine::Same(_)))
                .count();
            assert_eq!(from_old, old, "{old:?} -> {new:?}");
            assert_eq!(from_new, new, "{old:?} -> {new:?}");
            assert_eq!(same, lcs_len(&old, &new), "{old:?} -> {new:?}");
        }
    }

    #[test]
    fn line_diff_puts_the_removed_lines_before_the_added_ones() {
        assert_eq!(
            line_diff("a\nx\nb\nc\n", "a\ny\nb\nc\nz\n"),
            [
                DiffLine::Same("a"),
                DiffLine::Removed("x"),
                DiffLine::Added("y"),
                DiffLine::Same("b"),
                DiffLine::Same("c"),
                DiffLine::Added("z"),
            ]
        );
    }

    #[test]
    fn a_line_outside_the_new_content_counts_as_changed() {
        let lines = changed_lines("a\n", "a\n");
        assert!(!lines.contains(1));
        assert!(lines.contains(0));
        assert!(lines.contains(2));
    }

    #[test]
    fn an_element_on_a_line_unchanged_from_old_is_not_reported() {
        let old = "fn f() {\n    dbg!(1);\n}\n";
        let new = "fn f() {\n    dbg!(1);\n}\n\nfn g() {}\n";
        assert_eq!(check(old, new), CheckReport::Clean);
    }

    #[test]
    fn an_element_on_a_changed_line_is_reported_with_its_line_in_new() {
        let old = "fn f() {\n    dbg!(1);\n}\n";
        let new = "fn f() {\n    dbg!(1);\n}\n\nfn g() {\n    dbg!(2);\n}\n";
        assert_eq!(
            reported(&check(old, new)),
            [(6, ItemKey::Element(Element::DbgMacro))]
        );
    }

    #[test]
    fn when_old_is_empty_the_whole_file_is_checked() {
        let new = "fn f() {\n    dbg!(1);\n}\n\nfn g() {\n    dbg!(2);\n}\n";
        assert_eq!(
            reported(&check("", new)),
            [
                (2, ItemKey::Element(Element::DbgMacro)),
                (6, ItemKey::Element(Element::DbgMacro)),
            ]
        );
    }

    #[test]
    fn a_multi_line_element_whose_first_line_is_unchanged_is_not_reported() {
        let old = "fn f() {\n    tokio::select! {\n        a => 1,\n    }\n}\n";
        let new = "fn f() {\n    tokio::select! {\n        b => 2,\n    }\n}\n";
        assert_eq!(check(old, new), CheckReport::Clean);
    }

    #[test]
    fn a_multi_line_element_whose_first_line_is_changed_is_reported() {
        let old = "fn f() {\n    other! {\n        a => 1,\n    }\n}\n";
        let new = "fn f() {\n    tokio::select! {\n        a => 1,\n    }\n}\n";
        assert_eq!(
            reported(&check(old, new)),
            [
                (2, ItemKey::Element(Element::MacroCall)),
                (2, ItemKey::Crate("tokio".to_owned())),
            ]
        );
    }

    #[test]
    fn an_attribute_on_an_unchanged_line_above_a_changed_item_is_not_reported() {
        let old = "#[derive(Serialize)]\nstruct S {\n    a: u8,\n}\n";
        let new = "#[derive(Serialize)]\nstruct S {\n    a: u16,\n}\n";
        assert_eq!(check(old, new), CheckReport::Clean);
    }
}
