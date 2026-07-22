//! Concrete PHP values (the Singleton layer's inhabitants) and scalar bases.

use std::cmp::Ordering;

/// A scalar base type (the Refined/General layers' carrier).
///
/// `Null` is deliberately absent: nullability is a *flag* on the abstract
/// layers (a union with the one-inhabitant null type), and the null value
/// itself lives in the finite layers as [`Val::Null`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Base {
    /// PHP `int` (64-bit).
    Int,
    /// PHP `float`.
    Float,
    /// PHP `string`.
    String,
    /// PHP `bool`.
    Bool,
}

/// An array key after PHP normalization (the trace IR performs the
/// normalization; the domain only stores the result).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key {
    /// Integer key.
    Int(i64),
    /// String key.
    Str(String),
}

/// A concrete PHP value.
///
/// Equality and ordering are *representational*: floats compare by
/// [`f64::total_cmp`] (so `NAN == NAN` here, and `-0.0 != 0.0` — the domain
/// needs set semantics, not IEEE comparison; PHP-level `==`/`===` live in
/// the condition evaluator, not on this type).
#[derive(Debug, Clone)]
pub enum Val {
    /// Integer value.
    Int(i64),
    /// Float value (total order, see above).
    Float(f64),
    /// String value.
    Str(String),
    /// Boolean value.
    Bool(bool),
    /// The null value.
    Null,
    /// A fully-known array (normalized keys, in insertion order).
    Array(Vec<(Key, Val)>),
}

impl Val {
    /// The scalar base of this value, if it is a scalar.
    #[must_use]
    pub fn base(&self) -> Option<Base> {
        match self {
            Val::Int(_) => Some(Base::Int),
            Val::Float(_) => Some(Base::Float),
            Val::Str(_) => Some(Base::String),
            Val::Bool(_) => Some(Base::Bool),
            Val::Null | Val::Array(_) => None,
        }
    }

    fn discriminant_rank(&self) -> u8 {
        match self {
            Val::Null => 0,
            Val::Bool(_) => 1,
            Val::Int(_) => 2,
            Val::Float(_) => 3,
            Val::Str(_) => 4,
            Val::Array(_) => 5,
        }
    }
}

impl PartialEq for Val {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Val {}

impl PartialOrd for Val {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Val {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Val::Int(a), Val::Int(b)) => a.cmp(b),
            (Val::Float(a), Val::Float(b)) => a.total_cmp(b),
            (Val::Str(a), Val::Str(b)) => a.cmp(b),
            (Val::Bool(a), Val::Bool(b)) => a.cmp(b),
            (Val::Null, Val::Null) => Ordering::Equal,
            (Val::Array(a), Val::Array(b)) => a.cmp(b),
            _ => self.discriminant_rank().cmp(&other.discriminant_rank()),
        }
    }
}

impl std::hash::Hash for Val {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.discriminant_rank().hash(state);
        match self {
            Val::Int(v) => v.hash(state),
            Val::Float(v) => v.to_bits().hash(state),
            Val::Str(v) => v.hash(state),
            Val::Bool(v) => v.hash(state),
            Val::Null => {}
            Val::Array(items) => items.hash(state),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_order_is_consistent() {
        let vals = [
            Val::Null,
            Val::Bool(false),
            Val::Int(0),
            Val::Float(0.0),
            Val::Str(String::new()),
            Val::Array(vec![]),
        ];
        for a in &vals {
            assert_eq!(a.cmp(a), Ordering::Equal);
            for b in &vals {
                assert_eq!(a.cmp(b), b.cmp(a).reverse());
            }
        }
    }

    #[test]
    fn floats_are_set_like() {
        assert_eq!(Val::Float(f64::NAN), Val::Float(f64::NAN));
        assert_ne!(Val::Float(0.0), Val::Float(-0.0));
        assert_ne!(Val::Int(0), Val::Float(0.0));
    }
}
