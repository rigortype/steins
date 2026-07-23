//! [`EditPlan`] — an atomic transaction of non-overlapping `(file, span,
//! replacement)` edits plus new-file creations (ADR-0034 point 1).
//!
//! Built on span+splice (ADR-0003): untouched regions stay byte-identical by
//! construction, and overlapping edits are rejected at *planning* time (an
//! error, never a panic). The plan is JSON-serializable — it is the currency of
//! the dry-run → diff → approve loop.

use serde::{Deserialize, Serialize};

/// A byte-offset span into a file, `end`-exclusive. A serializable mirror of
/// [`steins_syntax::Span`] (which is not itself `Serialize`), so a plan can be
/// round-tripped through JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteSpan {
    pub start: u32,
    pub end: u32,
}

impl ByteSpan {
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// An insertion point: a zero-width span at `at`.
    #[must_use]
    pub const fn at(at: u32) -> Self {
        Self { start: at, end: at }
    }

    /// Whether two spans overlap. Adjacency (`[a, b)` then `[b, c)`) is **not**
    /// an overlap — two edits may meet at a point.
    #[must_use]
    pub const fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

impl From<steins_syntax::Span> for ByteSpan {
    fn from(s: steins_syntax::Span) -> Self {
        Self { start: s.start, end: s.end }
    }
}

/// A single byte-span replacement within one file. Deleting a region is a
/// replacement with an empty string; inserting is a zero-width span.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit {
    /// The file this edit applies to (the diagnostic path).
    pub path: String,
    pub span: ByteSpan,
    /// The bytes to splice in place of `span`.
    pub replacement: String,
}

/// A brand-new file the transaction creates (ADR-0034 point 1). Kept distinct
/// from an [`Edit`] because it has no existing bytes to splice into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewFile {
    pub path: String,
    pub contents: String,
}

/// Why a plan rejected an edit at planning time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// Two edits on the same file overlap. Carries the offending path and the
    /// two spans (in insertion order).
    Overlap { path: String, a: ByteSpan, b: ByteSpan },
    /// An edit's span is inverted (`start > end`).
    InvertedSpan { path: String, span: ByteSpan },
    /// A new file's path collides with an already-registered new file.
    DuplicateNewFile { path: String },
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanError::Overlap { path, a, b } => write!(
                f,
                "overlapping edits in {path}: [{}, {}) and [{}, {})",
                a.start, a.end, b.start, b.end
            ),
            PlanError::InvertedSpan { path, span } => {
                write!(f, "inverted edit span in {path}: [{}, {})", span.start, span.end)
            }
            PlanError::DuplicateNewFile { path } => {
                write!(f, "duplicate new-file creation: {path}")
            }
        }
    }
}

impl std::error::Error for PlanError {}

/// An atomic transaction of non-overlapping edits plus new-file creations
/// (ADR-0034 point 1). Overlap is rejected as edits are added, so a built plan
/// always splices cleanly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditPlan {
    pub edits: Vec<Edit>,
    pub new_files: Vec<NewFile>,
}

impl EditPlan {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the plan makes no changes at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty() && self.new_files.is_empty()
    }

    /// The set of file paths this plan edits (not including new files), in
    /// first-seen order.
    #[must_use]
    pub fn edited_paths(&self) -> Vec<&str> {
        let mut seen = Vec::new();
        for e in &self.edits {
            if !seen.contains(&e.path.as_str()) {
                seen.push(e.path.as_str());
            }
        }
        seen
    }

    /// Add an edit, rejecting an inverted span or an overlap with an
    /// already-registered edit on the same file (ADR-0034: overlaps are rejected
    /// at planning time, not at apply time).
    pub fn add_edit(&mut self, edit: Edit) -> Result<(), PlanError> {
        if edit.span.start > edit.span.end {
            return Err(PlanError::InvertedSpan { path: edit.path, span: edit.span });
        }
        for existing in &self.edits {
            if existing.path == edit.path && existing.span.overlaps(edit.span) {
                return Err(PlanError::Overlap {
                    path: edit.path,
                    a: existing.span,
                    b: edit.span,
                });
            }
        }
        self.edits.push(edit);
        Ok(())
    }

    /// Register a new file, rejecting a duplicate path.
    pub fn add_new_file(&mut self, file: NewFile) -> Result<(), PlanError> {
        if self.new_files.iter().any(|f| f.path == file.path) {
            return Err(PlanError::DuplicateNewFile { path: file.path });
        }
        self.new_files.push(file);
        Ok(())
    }

    /// Apply this plan's edits for a single file to its `source`, returning the
    /// rewritten text. Untouched byte regions are copied verbatim, so anything
    /// outside an edit span is preserved exactly (ADR-0003). Edits for other
    /// paths are ignored.
    ///
    /// # Panics
    /// Never in normal use: spans come from the parser at token boundaries, so
    /// they fall on UTF-8 char boundaries. A span past the end of `source`, or a
    /// mid-codepoint boundary, would panic on the slice — callers build plans
    /// from real spans, so this is a contract, not a runtime path.
    #[must_use]
    pub fn apply_file(&self, path: &str, source: &str) -> String {
        let mut spans: Vec<&Edit> = self.edits.iter().filter(|e| e.path == path).collect();
        spans.sort_by_key(|e| e.span.start);

        let mut out = String::with_capacity(source.len());
        let mut cursor = 0usize;
        for e in spans {
            let start = e.span.start as usize;
            let end = e.span.end as usize;
            // Copy the untouched run before this edit, then the replacement.
            out.push_str(&source[cursor..start]);
            out.push_str(&e.replacement);
            cursor = end;
        }
        out.push_str(&source[cursor..]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(path: &str, start: u32, end: u32, repl: &str) -> Edit {
        Edit { path: path.to_owned(), span: ByteSpan::new(start, end), replacement: repl.to_owned() }
    }

    #[test]
    fn single_replacement_splices_exactly() {
        let mut plan = EditPlan::new();
        // Replace "world" with "there" in "hello world".
        plan.add_edit(edit("a.php", 6, 11, "there")).unwrap();
        assert_eq!(plan.apply_file("a.php", "hello world"), "hello there");
    }

    #[test]
    fn insertion_is_zero_width() {
        let mut plan = EditPlan::new();
        // Insert "int " at offset 0.
        plan.add_edit(Edit {
            path: "a.php".into(),
            span: ByteSpan::at(0),
            replacement: "int ".into(),
        })
        .unwrap();
        assert_eq!(plan.apply_file("a.php", "$x"), "int $x");
    }

    #[test]
    fn multiple_edits_same_file_apply_in_order() {
        let mut plan = EditPlan::new();
        // Out-of-order insertion: apply must sort by start.
        plan.add_edit(edit("a.php", 6, 11, "there")).unwrap();
        plan.add_edit(edit("a.php", 0, 5, "howdy")).unwrap();
        assert_eq!(plan.apply_file("a.php", "hello world"), "howdy there");
    }

    #[test]
    fn adjacent_edits_are_not_an_overlap() {
        let mut plan = EditPlan::new();
        plan.add_edit(edit("a.php", 0, 5, "HELLO")).unwrap();
        // [5, 6) meets [0, 5) at a point — allowed.
        plan.add_edit(edit("a.php", 5, 6, "_")).unwrap();
        assert_eq!(plan.apply_file("a.php", "hello world"), "HELLO_world");
    }

    #[test]
    fn overlap_is_rejected_at_planning_time() {
        let mut plan = EditPlan::new();
        plan.add_edit(edit("a.php", 0, 5, "x")).unwrap();
        let err = plan.add_edit(edit("a.php", 3, 8, "y")).unwrap_err();
        assert!(matches!(err, PlanError::Overlap { .. }));
        // The rejected edit was not recorded.
        assert_eq!(plan.edits.len(), 1);
    }

    #[test]
    fn edits_on_different_files_never_overlap() {
        let mut plan = EditPlan::new();
        plan.add_edit(edit("a.php", 0, 5, "x")).unwrap();
        // Same span, different file — fine.
        plan.add_edit(edit("b.php", 0, 5, "y")).unwrap();
        assert_eq!(plan.edits.len(), 2);
    }

    #[test]
    fn inverted_span_is_rejected() {
        let mut plan = EditPlan::new();
        let err = plan.add_edit(edit("a.php", 8, 3, "z")).unwrap_err();
        assert!(matches!(err, PlanError::InvertedSpan { .. }));
    }

    #[test]
    fn deletion_removes_region() {
        let mut plan = EditPlan::new();
        // Delete " world".
        plan.add_edit(edit("a.php", 5, 11, "")).unwrap();
        assert_eq!(plan.apply_file("a.php", "hello world"), "hello");
    }

    #[test]
    fn multibyte_source_is_spliced_on_byte_offsets() {
        // "café = π" — 'é' is 2 bytes, 'π' is 2 bytes. Bytes:
        //   c(0) a(1) f(2) é(3,4) space(5) =(6) space(7) π(8,9)
        let source = "café = π";
        // Insert "int " at byte 8 (an ASCII/char boundary, right before π).
        let mut plan = EditPlan::new();
        plan.add_edit(Edit {
            path: "a.php".into(),
            span: ByteSpan::at(8),
            replacement: "int ".into(),
        })
        .unwrap();
        assert_eq!(plan.apply_file("a.php", source), "café = int π");
    }

    #[test]
    fn duplicate_new_file_is_rejected() {
        let mut plan = EditPlan::new();
        plan.add_new_file(NewFile { path: "n.php".into(), contents: "<?php".into() }).unwrap();
        let err = plan
            .add_new_file(NewFile { path: "n.php".into(), contents: "<?php // 2".into() })
            .unwrap_err();
        assert!(matches!(err, PlanError::DuplicateNewFile { .. }));
    }

    #[test]
    fn json_round_trip_preserves_the_plan() {
        let mut plan = EditPlan::new();
        plan.add_edit(edit("a.php", 6, 11, "there")).unwrap();
        plan.add_edit(edit("b.php", 0, 0, "int ")).unwrap();
        plan.add_new_file(NewFile { path: "n.php".into(), contents: "<?php".into() }).unwrap();

        let json = serde_json::to_string(&plan).unwrap();
        let back: EditPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan, back);
    }

    #[test]
    fn edited_paths_are_first_seen_order_without_duplicates() {
        let mut plan = EditPlan::new();
        plan.add_edit(edit("b.php", 0, 1, "x")).unwrap();
        plan.add_edit(edit("a.php", 0, 1, "y")).unwrap();
        plan.add_edit(edit("b.php", 5, 6, "z")).unwrap();
        assert_eq!(plan.edited_paths(), vec!["b.php", "a.php"]);
    }
}
