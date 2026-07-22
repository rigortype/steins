//! The unified trinary judgment (ADR-0031): PHPStan's TrinaryLogic and
//! Rigor's Certainty are the same lattice; Steins has exactly one.

/// A trinary judgment: `Yes` / `No` / `Maybe`.
///
/// `Maybe` never promotes: combining evidence can only move *toward* `Maybe`
/// (via [`Certainty::and`]/[`Certainty::or`] mixing), never conjure a `Yes`
/// from repetition — the discipline imported from Rigor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Certainty {
    /// Provably true.
    Yes,
    /// Provably false.
    No,
    /// Not decided; the honest middle. Silence-producing in the proof layer.
    Maybe,
}

impl Certainty {
    /// Lift a decided boolean.
    #[must_use]
    pub const fn from_bool(b: bool) -> Self {
        if b { Certainty::Yes } else { Certainty::No }
    }

    /// Lift a possibly-undecided boolean: `None` is `Maybe`.
    #[must_use]
    pub const fn from_opt(b: Option<bool>) -> Self {
        match b {
            Some(v) => Certainty::from_bool(v),
            None => Certainty::Maybe,
        }
    }

    /// Three-valued conjunction (Kleene strong logic).
    #[must_use]
    pub const fn and(self, other: Self) -> Self {
        match (self, other) {
            (Certainty::No, _) | (_, Certainty::No) => Certainty::No,
            (Certainty::Yes, Certainty::Yes) => Certainty::Yes,
            _ => Certainty::Maybe,
        }
    }

    /// Three-valued disjunction (Kleene strong logic).
    #[must_use]
    pub const fn or(self, other: Self) -> Self {
        match (self, other) {
            (Certainty::Yes, _) | (_, Certainty::Yes) => Certainty::Yes,
            (Certainty::No, Certainty::No) => Certainty::No,
            _ => Certainty::Maybe,
        }
    }

    /// Three-valued negation.
    #[must_use]
    pub const fn not(self) -> Self {
        match self {
            Certainty::Yes => Certainty::No,
            Certainty::No => Certainty::Yes,
            Certainty::Maybe => Certainty::Maybe,
        }
    }

    /// `true` iff `Yes`.
    #[must_use]
    pub const fn is_yes(self) -> bool {
        matches!(self, Certainty::Yes)
    }

    /// `true` iff `No`.
    #[must_use]
    pub const fn is_no(self) -> bool {
        matches!(self, Certainty::No)
    }

    /// Fold a collection of judgments about *every* member: all `Yes` → `Yes`,
    /// all `No` → `No`, anything mixed or `Maybe` → `Maybe`.
    #[must_use]
    pub fn all_of<I: IntoIterator<Item = Certainty>>(items: I) -> Self {
        let mut iter = items.into_iter();
        let Some(first) = iter.next() else {
            // Vacuous truth is a trap: an empty set decides nothing.
            return Certainty::Maybe;
        };
        if matches!(first, Certainty::Maybe) {
            return Certainty::Maybe;
        }
        for c in iter {
            if c != first {
                return Certainty::Maybe;
            }
        }
        first
    }
}

#[cfg(test)]
mod tests {
    use super::Certainty::{Maybe, No, Yes};
    use super::*;

    #[test]
    fn kleene_tables() {
        assert_eq!(Yes.and(Maybe), Maybe);
        assert_eq!(No.and(Maybe), No);
        assert_eq!(Yes.or(Maybe), Yes);
        assert_eq!(No.or(Maybe), Maybe);
        assert_eq!(Maybe.not(), Maybe);
    }

    #[test]
    fn all_of_folds() {
        assert_eq!(Certainty::all_of([Yes, Yes]), Yes);
        assert_eq!(Certainty::all_of([No, No]), No);
        assert_eq!(Certainty::all_of([Yes, No]), Maybe);
        assert_eq!(Certainty::all_of([]), Maybe);
    }
}
