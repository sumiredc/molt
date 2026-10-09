//! The diff of a file molt changed, as the user reads it: a unified diff with 3 lines of context.

use crate::checker::diff::{DiffLine, line_diff};

/// Lines of context around each change.
const CONTEXT: usize = 3;

/// The unified diff of `path` from `before` (`None` for a file molt created) to `after`. Empty
/// when the contents have the same lines.
pub(super) fn unified(path: &str, before: Option<&str>, after: &str) -> String {
    let lines = line_diff(before.unwrap_or(""), after);
    let changed: Vec<usize> = (0..lines.len())
        .filter(|&i| !matches!(lines[i], DiffLine::Same(_)))
        .collect();
    if changed.is_empty() {
        return String::new();
    }
    let mut out = match before {
        Some(_) => format!("--- a/{path}\n+++ b/{path}\n"),
        None => format!("--- /dev/null\n+++ b/{path}\n"),
    };
    // A hunk is a run of changes at most 2 * CONTEXT unchanged lines apart, with its context.
    let mut hunks: Vec<(usize, usize)> = Vec::new();
    for &i in &changed {
        let start = i.saturating_sub(CONTEXT);
        let end = (i + CONTEXT + 1).min(lines.len());
        match hunks.last_mut() {
            Some((_, last_end)) if start <= *last_end => *last_end = end,
            _ => hunks.push((start, end)),
        }
    }
    let in_old = |line: &&DiffLine| !matches!(line, DiffLine::Added(_));
    let in_new = |line: &&DiffLine| !matches!(line, DiffLine::Removed(_));
    for (start, end) in hunks {
        let hunk = &lines[start..end];
        out.push_str(&format!(
            "@@ -{} +{} @@\n",
            range(
                lines[..start].iter().filter(in_old).count(),
                hunk.iter().filter(in_old).count()
            ),
            range(
                lines[..start].iter().filter(in_new).count(),
                hunk.iter().filter(in_new).count()
            ),
        ));
        for line in hunk {
            let (mark, text) = match line {
                DiffLine::Same(text) => (' ', text),
                DiffLine::Removed(text) => ('-', text),
                DiffLine::Added(text) => ('+', text),
            };
            out.push(mark);
            out.push_str(text);
            out.push('\n');
        }
    }
    out.truncate(out.trim_end_matches('\n').len());
    out
}

/// A hunk's range in one content as a unified diff writes it, where `before` lines of that
/// content precede the hunk: `start,count`, or only `start` for one line. An empty range is
/// written at the line before it.
fn range(before: usize, count: usize) -> String {
    match count {
        0 => format!("{before},0"),
        1 => format!("{}", before + 1),
        _ => format!("{},{count}", before + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(count: usize) -> String {
        (1..=count).map(|n| format!("line {n}\n")).collect()
    }

    #[test]
    fn a_new_file_is_all_added_lines_from_dev_null() {
        assert_eq!(
            unified("src/lib.rs", None, "pub fn f() {}\n"),
            "--- /dev/null\n+++ b/src/lib.rs\n@@ -0,0 +1 @@\n+pub fn f() {}"
        );
    }

    #[test]
    fn a_change_shows_with_three_lines_of_context_and_its_line_numbers() {
        let before = numbered(9);
        let after = before.replace("line 5\n", "five\n");
        assert_eq!(
            unified("src/main.rs", Some(&before), &after),
            "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -2,7 +2,7 @@\n line 2\n line 3\n line 4\n-line 5\n+five\n line 6\n line 7\n line 8"
        );
    }

    #[test]
    fn changes_far_apart_are_separate_hunks() {
        let before = numbered(20);
        let after = before
            .replace("line 2\n", "two\n")
            .replace("line 19\n", "nineteen\n");
        let diff = unified("a.rs", Some(&before), &after);
        assert_eq!(diff.matches("@@ -").count(), 2, "{diff}");
        assert!(diff.contains("@@ -1,5 +1,5 @@"), "{diff}");
        assert!(diff.contains("@@ -16,5 +16,5 @@"), "{diff}");
    }

    #[test]
    fn changes_close_together_are_one_hunk() {
        let before = numbered(20);
        let after = before
            .replace("line 5\n", "five\n")
            .replace("line 11\n", "eleven\n");
        let diff = unified("a.rs", Some(&before), &after);
        assert_eq!(diff.matches("@@ -").count(), 1, "{diff}");
        assert!(diff.contains("@@ -2,13 +2,13 @@"), "{diff}");
    }

    #[test]
    fn the_same_content_has_no_diff() {
        assert_eq!(unified("a.rs", Some("x\n"), "x\n"), "");
    }
}
