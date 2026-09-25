//! The whole-project effect and throw fixpoints of one check run (issue #489,
//! ADR-0092 §5): the call-graph node they are keyed by ([`Sym`]), the three
//! textual gates that decide whether either is computed at all ([`Gate`]), and
//! the lazy holder every consumer in the run reads ([`Fixpoints`]).
//!
//! [`Sym`] is re-exported at the crate root with the other two: the effects
//! pass, the throw system, the escape sweep, the per-file facts and [`Cx`]'s
//! purity question all key on it.
//!
//! [`Cx`]: crate::cx::Cx

use std::collections::HashMap;

use steins_db::{EffectsPolicy, PluginFacts};

use crate::project::{FileUnit, Index};
use crate::{clock, facts, ms, purity, throws};

/// Which of the three whole-universe textual gates [`Fixpoints::any`] asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gate {
    Purity,
    Envelope,
    Throws,
}

/// A node in the unified project effect call graph — a free function (keyed by
/// FQN) or a class method (keyed by class FQN + method name).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    derive(serde::Serialize, serde::Deserialize),
    serde(deny_unknown_fields)
)]
pub(crate) enum Sym {
    Func(String),
    Method(String, String),
    /// A closure/arrow body (ADR-0033), keyed by file path + definition-site
    /// offset (closures are same-file, so this key is stable within a project).
    Closure(String, u32),
}

/// The whole-project effect and throw fixpoint results of ONE check run,
/// computed at most once each (issue #489 / ADR-0092 §5).
///
/// Before this holder, [`check_units`] ran the effect fixpoint inside every
/// consumer that wanted it — `PurityOracle::build` and `effect_diagnostics`
/// each computed their own copy, and `throw_diagnostics` its own throw
/// fixpoint. The fixpoints are deterministic and order-independent (ADR-0048
/// §4), so those copies were byte-identical; this makes the sharing structural:
/// one producer per run, every internal consumer reads the same value.
///
/// Laziness is load-bearing, not an optimization nicety: each consumer keeps
/// its own cheap textual gate (a project with no envelope, no purity-bearing
/// callable and no `@throws` never pays for a fixpoint at all), and the holder
/// computes on the first gate that passes.
///
/// Standalone library entry points (`effect_summary`, `region_purity_project`,
/// `sweep_escapes`, the JSON effect surface) run outside a check and keep
/// computing their own copy — determinism makes those equal by construction.
///
/// [`check_units`]: crate::check_units
pub(crate) struct Fixpoints<'a> {
    units: &'a [FileUnit<'a>],
    index: &'a Index,
    plugins: &'a PluginFacts,
    policy: &'a EffectsPolicy,
    /// This run's per-file facts, in unit order — empty on every path but the
    /// generation orchestrator's (issue #516). Where a file has them, its own
    /// rows come from there and its tree is never decoded; where it does not,
    /// the classifier reads the tree exactly as it always did.
    facts: &'a [facts::FileFacts],
    effects: std::cell::OnceCell<HashMap<Sym, purity::EffectSet>>,
    throws: std::cell::OnceCell<HashMap<Sym, throws::ThrowSet>>,
    /// Wall-clock milliseconds each fixpoint cost, recorded at the one place
    /// each is computed (issue #516 asks where the warm run's remaining time
    /// goes, and "analyze" was one undifferentiated number). Zero for a
    /// fixpoint no consumer's gate ever forced.
    spent: std::cell::Cell<(f64, f64)>,
}

impl<'a> Fixpoints<'a> {
    pub(crate) fn new(
        units: &'a [FileUnit<'a>],
        index: &'a Index,
        plugins: &'a PluginFacts,
        policy: &'a EffectsPolicy,
        facts: &'a [facts::FileFacts],
    ) -> Self {
        Self {
            units,
            index,
            plugins,
            policy,
            facts,
            effects: std::cell::OnceCell::new(),
            throws: std::cell::OnceCell::new(),
            spent: std::cell::Cell::new((0.0, 0.0)),
        }
    }

    /// `(effects, throws)` fixpoint milliseconds — see [`Self::spent`].
    pub(crate) fn spent(&self) -> (f64, f64) {
        self.spent.get()
    }

    pub(crate) fn units(&self) -> &'a [FileUnit<'a>] {
        self.units
    }

    pub(crate) fn index(&self) -> &'a Index {
        self.index
    }

    pub(crate) fn plugins(&self) -> &'a PluginFacts {
        self.plugins
    }

    pub(crate) fn policy(&self) -> &'a EffectsPolicy {
        self.policy
    }

    /// Whether **any** declaration in the universe spells a purity-bearing
    /// callable, an effect envelope or an interop one, or `@throws` — the three
    /// cheap textual gates that decide whether a fixpoint runs at all.
    ///
    /// Read off the per-file facts where the run has them, so a project that
    /// spells none of the three answers `false` without decoding a tree (issue
    /// #516: this gate alone used to force the whole universe).
    pub(crate) fn any(&self, gate: Gate) -> bool {
        (0..self.units.len()).any(|fi| self.spells(fi, gate))
    }

    /// The same question for one file — what `throw_diagnostics` skips on.
    pub(crate) fn spells(&self, fi: usize, gate: Gate) -> bool {
        if let Some(facts) = self.facts.get(fi) {
            return match gate {
                Gate::Purity => facts.spells_purity,
                Gate::Envelope => facts.spells_envelope,
                Gate::Throws => facts.spells_throws,
            };
        }
        let tree = self.units[fi].tree;
        let doc = |doc: Option<&String>| match gate {
            Gate::Purity => {
                doc.is_some_and(|t| t.contains("pure-callable") || t.contains("pure-closure"))
            }
            Gate::Envelope => purity::spells_interop_envelope(doc),
            Gate::Throws => doc.is_some_and(|t| t.contains("throws")),
        };
        let envelope = matches!(gate, Gate::Envelope);
        tree.functions()
            .iter()
            .any(|f| doc(f.docblock.as_ref()) || (envelope && f.effect_envelope.is_some()))
            || tree.classes().iter().any(|c| {
                (envelope && purity::spells_interop_envelope(c.docblock.as_ref()))
                    || c.methods.iter().any(|m| {
                        doc(m.docblock.as_ref()) || (envelope && m.effect_envelope.is_some())
                    })
            })
    }

    /// The effect fixpoint result, computed on first request.
    pub(crate) fn effects(&self) -> &HashMap<Sym, purity::EffectSet> {
        self.effects.get_or_init(|| {
            let t = clock();
            let out = purity::compute_effects(
                self.units,
                self.index,
                self.plugins,
                self.policy,
                self.facts,
            );
            let (_, throws) = self.spent.get();
            self.spent.set((ms(t), throws));
            out
        })
    }

    /// The throw fixpoint result, computed on first request.
    pub(crate) fn throws(&self) -> &HashMap<Sym, throws::ThrowSet> {
        self.throws.get_or_init(|| {
            let t = clock();
            let out = throws::compute_throws(self.units, self.index, self.facts);
            let (effects, _) = self.spent.get();
            self.spent.set((effects, ms(t)));
            out
        })
    }
}
