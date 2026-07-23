//! The transform vocabulary (ADR-0034 points 2 & 3): first-class [`Refusal`]s,
//! and a [`CompletenessOracle`] that accounts every enumerated site as
//! transformed-or-refused so nothing is silently dropped.
//!
//! A transform's real currency is per-site, not whole-plan: a run over a project
//! promotes some sites and refuses others, each refusal carrying a *named*
//! reason an agent can read and act on (ADR-0034 point 2). [`TransformReport`]
//! bundles the [`EditPlan`], the refusals, and the oracle.

use serde::{Deserialize, Serialize};

use crate::EditPlan;

/// Where a transform decision landed: a file position plus a human label of the
/// site (`function f() param $x`, `call to f() at …`). Used by both refusals and
/// the audit trail of what was transformed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteRef {
    pub path: String,
    pub line: u32,
    pub column: u32,
    /// A short human label identifying the site.
    pub label: String,
}

impl SiteRef {
    #[must_use]
    pub fn new(path: impl Into<String>, line: u32, column: u32, label: impl Into<String>) -> Self {
        Self { path: path.into(), line, column, label: label.into() }
    }
}

/// A first-class refusal (ADR-0034 point 2): the Certainty discipline applied to
/// rewriting. `reason` is a stable machine-readable name (`dynamic-call-present`,
/// `argument-not-proven`, …); `detail` is the human sentence the agent reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    /// The candidate site the refusal is *about* (the promotion target).
    pub site: SiteRef,
    /// A stable, named reason. See the phpdoc-promotion module for the catalog.
    pub reason: String,
    /// Human-readable detail — enough for an agent to continue the conversation.
    pub detail: String,
}

impl Refusal {
    #[must_use]
    pub fn new(site: SiteRef, reason: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { site, reason: reason.into(), detail: detail.into() }
    }
}

/// The completeness oracle (ADR-0034 point 3b): every enumerated candidate site
/// is accounted for as transformed or refused — a mismatch is a bug in the
/// transform, surfaced by [`Self::is_complete`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletenessOracle {
    /// Candidate sites the enumerator produced.
    pub enumerated: usize,
    /// Sites that produced an edit.
    pub transformed: usize,
    /// Sites that produced a refusal.
    pub refused: usize,
}

impl CompletenessOracle {
    /// Whether every enumerated site was accounted for (transformed + refused ==
    /// enumerated). A `false` here means the transform dropped a site silently —
    /// an internal invariant violation, not a user-facing state.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.transformed + self.refused == self.enumerated
    }
}

/// The full result of running a transform over a project: the atomic
/// [`EditPlan`], the per-site refusals, and the completeness oracle.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransformReport {
    pub plan: EditPlan,
    pub refusals: Vec<Refusal>,
    pub oracle: CompletenessOracle,
}

/// The transform contract (ADR-0034 point 2). Concrete transforms (e.g.
/// phpdoc→native promotion) carry their own project context and produce a
/// [`TransformReport`]; this trait names the transform and its stable id so the
/// CLI/MCP surface can dispatch generically.
pub trait Transform {
    /// The stable command id, e.g. `"phpdoc-to-native"`.
    fn id(&self) -> &'static str;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oracle_completeness() {
        let o = CompletenessOracle { enumerated: 5, transformed: 2, refused: 3 };
        assert!(o.is_complete());
        let bad = CompletenessOracle { enumerated: 5, transformed: 2, refused: 2 };
        assert!(!bad.is_complete());
    }

    #[test]
    fn report_json_round_trip() {
        let report = TransformReport {
            plan: EditPlan::new(),
            refusals: vec![Refusal::new(
                SiteRef::new("a.php", 3, 5, "function f() param $x"),
                "dynamic-call-present",
                "a $fn(...) call could target f()",
            )],
            oracle: CompletenessOracle { enumerated: 1, transformed: 0, refused: 1 },
        };
        let json = serde_json::to_string(&report).unwrap();
        let back: TransformReport = serde_json::from_str(&json).unwrap();
        assert_eq!(report, back);
    }
}
