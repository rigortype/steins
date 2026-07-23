//! A minimal, dependency-free unified-diff renderer for dry-run output.
//!
//! Line-based LCS (no third-party diff crate — the workspace carries none, and
//! ADR-0034 wants the dry-run diff cheap and self-contained). Good enough for
//! the small, localized EditPlans this engine produces; it is a *display*
//! artifact, never the source of truth (the [`crate::EditPlan`] is).

/// Render a unified diff of `old` → `new` for a file named `path`, with
/// `context` unchanged lines around each change. Returns an empty string when
/// the two texts are identical.
#[must_use]
pub fn unified_diff(path: &str, old: &str, new: &str, context: usize) -> String {
    if old == new {
        return String::new();
    }
    let old_lines: Vec<&str> = split_lines(old);
    let new_lines: Vec<&str> = split_lines(new);
    let ops = diff_ops(&old_lines, &new_lines);
    let hunks = group_hunks(&ops, context);
    if hunks.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    out.push_str(&format!("--- a/{path}\n"));
    out.push_str(&format!("+++ b/{path}\n"));
    for h in hunks {
        out.push_str(&h.render(&old_lines, &new_lines));
    }
    out
}

/// Split into lines *without* the trailing newline, treating a final newline as
/// a line terminator (so "a\nb\n" is two lines, "a\nb" is two lines too). This
/// keeps hunk line counts intuitive for the common newline-terminated file.
fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    // A trailing '\n' yields a spurious empty final element; drop it.
    if text.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// One line-level edit operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// A line present in both (indices into old, new).
    Equal(usize, usize),
    /// A line only in old.
    Delete(usize),
    /// A line only in new.
    Insert(usize),
}

/// Longest-common-subsequence diff over line indices (classic DP backtrace).
fn diff_ops(old: &[&str], new: &[&str]) -> Vec<Op> {
    let (n, m) = (old.len(), new.len());
    // lcs[i][j] = LCS length of old[i..], new[j..].
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut ops = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if old[i] == new[j] {
            ops.push(Op::Equal(i, j));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            ops.push(Op::Delete(i));
            i += 1;
        } else {
            ops.push(Op::Insert(j));
            j += 1;
        }
    }
    while i < n {
        ops.push(Op::Delete(i));
        i += 1;
    }
    while j < m {
        ops.push(Op::Insert(j));
        j += 1;
    }
    ops
}

/// A contiguous run of ops forming one `@@ … @@` hunk.
struct Hunk {
    old_start: usize,
    old_len: usize,
    new_start: usize,
    new_len: usize,
    ops: Vec<Op>,
}

impl Hunk {
    fn render(&self, old: &[&str], new: &[&str]) -> String {
        let mut s = format!(
            "@@ -{},{} +{},{} @@\n",
            self.old_start + 1,
            self.old_len,
            self.new_start + 1,
            self.new_len
        );
        for op in &self.ops {
            match *op {
                Op::Equal(i, _) => s.push_str(&format!(" {}\n", old[i])),
                Op::Delete(i) => s.push_str(&format!("-{}\n", old[i])),
                Op::Insert(j) => s.push_str(&format!("+{}\n", new[j])),
            }
        }
        s
    }
}

/// Group the flat op stream into hunks, keeping `context` equal lines on each
/// side of a change and merging changes that fall within `2 * context` of each
/// other into one hunk.
fn group_hunks(ops: &[Op], context: usize) -> Vec<Hunk> {
    // Indices of ops that are actual changes.
    let change_positions: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, o)| !matches!(o, Op::Equal(..)))
        .map(|(k, _)| k)
        .collect();
    if change_positions.is_empty() {
        return Vec::new();
    }

    // Build [lo, hi) op-index windows around change clusters, merging overlaps.
    let mut windows: Vec<(usize, usize)> = Vec::new();
    for &p in &change_positions {
        let lo = p.saturating_sub(context);
        let hi = (p + context + 1).min(ops.len());
        match windows.last_mut() {
            Some(last) if lo <= last.1 => last.1 = last.1.max(hi),
            _ => windows.push((lo, hi)),
        }
    }

    windows
        .into_iter()
        .map(|(lo, hi)| build_hunk(&ops[lo..hi]))
        .collect()
}

/// Materialize one hunk from a contiguous op slice, computing its old/new line
/// ranges from the first/last real line indices the slice touches.
fn build_hunk(slice: &[Op]) -> Hunk {
    let mut old_start = None;
    let mut new_start = None;
    let (mut old_len, mut new_len) = (0usize, 0usize);
    for op in slice {
        match *op {
            Op::Equal(i, j) => {
                old_start.get_or_insert(i);
                new_start.get_or_insert(j);
                old_len += 1;
                new_len += 1;
            }
            Op::Delete(i) => {
                old_start.get_or_insert(i);
                old_len += 1;
            }
            Op::Insert(j) => {
                new_start.get_or_insert(j);
                new_len += 1;
            }
        }
    }
    Hunk {
        old_start: old_start.unwrap_or(0),
        old_len,
        new_start: new_start.unwrap_or(0),
        new_len,
        ops: slice.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_texts_produce_no_diff() {
        assert_eq!(unified_diff("a.php", "same\n", "same\n", 3), "");
    }

    #[test]
    fn single_line_change_renders_a_hunk() {
        let old = "line1\nline2\nline3\n";
        let new = "line1\nCHANGED\nline3\n";
        let d = unified_diff("a.php", old, new, 3);
        assert!(d.contains("--- a/a.php"));
        assert!(d.contains("+++ b/a.php"));
        assert!(d.contains("-line2"));
        assert!(d.contains("+CHANGED"));
        assert!(d.contains(" line1"));
        assert!(d.contains(" line3"));
    }

    #[test]
    fn insertion_only() {
        let old = "a\nb\n";
        let new = "a\nNEW\nb\n";
        let d = unified_diff("f", old, new, 3);
        assert!(d.contains("+NEW"));
        assert!(!d.contains("-a"));
        assert!(!d.contains("-b"));
    }

    #[test]
    fn deletion_only() {
        let old = "a\ngone\nb\n";
        let new = "a\nb\n";
        let d = unified_diff("f", old, new, 3);
        assert!(d.contains("-gone"));
    }

    #[test]
    fn hunk_header_counts_are_correct() {
        // 3 old lines, 1 changed → header covers all with context.
        let old = "a\nb\nc\n";
        let new = "a\nB\nc\n";
        let d = unified_diff("f", old, new, 3);
        assert!(d.contains("@@ -1,3 +1,3 @@"), "diff was:\n{d}");
    }

    #[test]
    fn distant_changes_form_separate_hunks() {
        let old = (1..=20).map(|n| format!("l{n}")).collect::<Vec<_>>().join("\n") + "\n";
        let mut new_lines: Vec<String> = (1..=20).map(|n| format!("l{n}")).collect();
        new_lines[1] = "CHANGED2".into();
        new_lines[18] = "CHANGED19".into();
        let new = new_lines.join("\n") + "\n";
        let d = unified_diff("f", &old, &new, 3);
        // Two separate @@ headers.
        assert_eq!(d.matches("@@ ").count(), 2, "diff was:\n{d}");
    }

    #[test]
    fn diff_applies_context_only_around_changes() {
        let old = (1..=20).map(|n| format!("l{n}")).collect::<Vec<_>>().join("\n") + "\n";
        let mut new_lines: Vec<String> = (1..=20).map(|n| format!("l{n}")).collect();
        new_lines[9] = "TEN".into();
        let new = new_lines.join("\n") + "\n";
        let d = unified_diff("f", &old, &new, 2);
        // Context of 2 means l8,l9 before and l11,l12 after (not l1 or l20).
        assert!(d.contains(" l8"));
        assert!(d.contains("-l10"));
        assert!(d.contains("+TEN"));
        assert!(d.contains(" l12"));
        assert!(!d.contains(" l1\n"));
        assert!(!d.contains(" l20"));
    }
}
