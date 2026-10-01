//! A project function's conditional-purity contracts (ADR-0063 §2 decision 2),
//! decided at one call site: the userland catalog row a declaration writes in a
//! docblock instead of the catalog curating it.

use steins_phpdoc::{TagKind, scan_docblock};

use super::engine::by_ref_label;

/// A function's declared **conditional-purity** contracts (ADR-0063 §2 decision 2),
/// resolved from parameter names to positional indices.
#[derive(Debug, Default, Clone)]
pub(super) struct ConditionalPurity {
    /// Positions flagged by `@pure-unless-callable-is-impure $cb`: this
    /// function's envelope is the join of the callables bound here.
    callables: Vec<usize>,
    /// Positions flagged by `@pure-unless-parameter-passed $out`: this function
    /// is pure unless the argument is supplied. The declarative twin of a catalog
    /// out-param row — a userland row, written by the author instead of curated.
    passed: Vec<usize>,
}

impl ConditionalPurity {
    fn is_empty(&self) -> bool {
        self.callables.is_empty() && self.passed.is_empty()
    }
}

/// Read a declaration's conditional-purity tags, mapping each flagged parameter
/// name to its positional index. `None` when the docblock declares none.
///
/// A tag naming a parameter the signature does not have is dropped, not
/// diagnosed: the crate's tag discipline is that a malformed or stale tag costs
/// its own effect and nothing else.
pub(super) fn conditional_purity(docblock: Option<&String>, params: &[steins_syntax::Param]) -> Option<ConditionalPurity> {
    let text = docblock?;
    // Cheap gate: both spellings share this substring, and it is vanishingly rare
    // in prose. Scanning every docblock in the project would not be.
    if !text.contains("pure-unless") {
        return None;
    }
    let mut cp = ConditionalPurity::default();
    for tag in scan_docblock(text) {
        let TagKind::ConditionalPurity(cond) = tag.kind else { continue };
        let Some(var) = &tag.var_name else { continue };
        let name = var.trim_start_matches('$');
        let Some(pos) = params.iter().position(|p| p.name == name) else { continue };
        let slot = match cond {
            steins_phpdoc::PurityCondition::CallableIsImpure => &mut cp.callables,
            steins_phpdoc::PurityCondition::ParameterIsPassed => &mut cp.passed,
        };
        if !slot.contains(&pos) {
            slot.push(pos);
        }
    }
    (!cp.is_empty()).then_some(cp)
}

/// How a call to a **user** function contributes to the caller's effect set, once
/// the callee's conditional-purity contracts (ADR-0063 §2 decision 2) are honored.
pub(super) struct UserCallEffects {
    /// Whether the callee's *exhaustiveness taint* is discharged by its contract.
    ///
    /// A tagged function's body calls its callable parameter dynamically
    /// (`$cb(...)`), which is a dynamic site (`SiteKind::Dynamic`) and taints the callee
    /// forever — the very unprovability the contract exists to answer. When every
    /// flagged condition is decided at this call site (the callable is a
    /// resolvable callback, or the flagged argument is simply absent), the
    /// declaration discharges that taint.
    ///
    /// This does not invert ADR-0037's "proven beats declared": every finding the
    /// fixpoint *proved* about the callee still propagates. A declaration is only
    /// permitted to answer what inference left unknown.
    pub(super) discharge_taint: bool,
    /// Labels the call contributes directly — the `@pure-unless-parameter-passed`
    /// leg, resolved against the argument's lvalue root exactly as a catalog
    /// out-param row would be.
    pub(super) labels: Vec<&'static str>,
}

/// Evaluate a user callee's conditional-purity contracts against one call site.
///
/// `callbacks` are the resolvable callback arguments by position (empty for a
/// plain call); `arg_targets` is `None` when positional mapping
/// was defeated, in which case no condition can be evaluated and nothing is
/// discharged.
pub(super) fn eval_conditional_purity(
    cp: &ConditionalPurity,
    callbacks: &[(usize, steins_syntax::CallbackRef)],
    arg_targets: Option<&[steins_syntax::RefTarget]>,
    mut on_callback: impl FnMut(&steins_syntax::CallbackRef),
) -> UserCallEffects {
    let Some(targets) = arg_targets else {
        return UserCallEffects { discharge_taint: false, labels: Vec::new() };
    };
    let arity = targets.len();
    let mut discharge = true;
    let mut labels: Vec<&'static str> = Vec::new();
    for &p in &cp.callables {
        // Not supplied → the condition is vacuous and the function is pure.
        if p >= arity {
            continue;
        }
        match callbacks.iter().find(|(q, _)| *q == p) {
            // Visible callback: its envelope joins the caller's (ADR-0063
            // decision 1's semantic answer, reached through the declaration).
            Some((_, cbref)) => on_callback(cbref),
            // An opaque `callable` sits in the flagged slot — precisely the case
            // the contract cannot resolve either. The taint stands, as today.
            None => discharge = false,
        }
    }
    for &p in &cp.passed {
        let Some(&target) = targets.get(p) else { continue };
        let label = by_ref_label(target);
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    UserCallEffects { discharge_taint: discharge, labels }
}

