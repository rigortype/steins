//! The method-call value carrier's LOWERING (issue #386).
//!
//! `ArgValue::MethodCall` carries the statement vocabulary — the same [`Callee`]
//! a trace `CallExpr` holds — so a call written as an argument denotes exactly
//! what the same call written as a statement denotes. This file pins the
//! boundary: which spellings the carrier takes, which still widen to `Other`,
//! and that `Receiver::New` now carries the constructor's arguments.

use steins_syntax::{ArgValue, Callee, NameRef, Receiver, SourceTree, StaticClass, StmtKind};

/// The value assigned to `$x` by the first assignment.
fn lowered(expr: &str) -> ArgValue {
    let src = format!("<?php\n$x = {expr};\n");
    let tree = SourceTree::parse(&src);
    tree.scopes()
        .iter()
        .find_map(|s| {
            s.stmts.iter().find_map(|st| match &st.kind {
                StmtKind::Assign { var, value, .. } if var == "x" => Some(value.clone()),
                _ => None,
            })
        })
        .expect("an assignment")
}

/// The callee of a lowered method-call value.
fn callee(expr: &str) -> Callee {
    match lowered(expr) {
        ArgValue::MethodCall { callee, .. } => callee,
        other => panic!("`{expr}` lowered to {other:?}"),
    }
}

#[test]
fn every_statically_named_receiver_form_lowers_to_the_carrier() {
    assert!(matches!(
        callee("$b->m()"),
        Callee::Method { receiver: Receiver::Var(v), method, nullsafe: false } if v == "b" && method == "m"
    ));
    assert!(matches!(
        callee("$this->m()"),
        Callee::Method { receiver: Receiver::This, method, nullsafe: false } if method == "m"
    ));
    assert!(matches!(
        callee("$b?->m()"),
        Callee::Method { receiver: Receiver::Var(_), nullsafe: true, .. }
    ));
    // A depth-1 property receiver is carried; it is the RESOLUTION that declines
    // it (ADR-0052 §7), which is why the carrier does not have to.
    assert!(matches!(
        callee("$a->p->m()"),
        Callee::Method { receiver: Receiver::Prop { var, prop }, .. } if var == "a" && prop == "p"
    ));
    assert!(matches!(
        callee("Foo::m()"),
        Callee::Static { class: StaticClass::Named(_), method } if method == "m"
    ));
    for (src, want) in [
        ("self::m()", StaticClass::SelfKw),
        ("parent::m()", StaticClass::Parent),
        ("static::m()", StaticClass::Static),
    ] {
        assert!(matches!(callee(src), Callee::Static { class, .. } if class == want));
    }
}

#[test]
fn a_receiver_new_carries_the_constructors_arguments() {
    // The half issue #374 measured missing: `Receiver::New` used to be the class
    // reference alone, so the object the call dispatches on could not be minted.
    match callee("(new C(1, 's'))->m()") {
        Callee::Method { receiver: Receiver::New { class, args, named }, .. } => {
            assert_eq!(class.simple(), "C");
            assert_eq!(args, vec![ArgValue::Int(1), ArgValue::Str("s".into())]);
            assert!(named.is_empty());
        }
        other => panic!("expected a new receiver, got {other:?}"),
    }
    // A named argument travels too — the descent declines it, the carrier does not.
    match callee("(new C(n: 1))->m()") {
        Callee::Method { receiver: Receiver::New { args, named, .. }, .. } => {
            assert!(args.is_empty());
            assert_eq!(named.len(), 1);
            assert_eq!(named[0].name, "n");
        }
        other => panic!("expected a new receiver, got {other:?}"),
    }
    // `new C` with no argument list at all is the zero-argument call.
    match callee("(new C)->m()") {
        Callee::Method { receiver: Receiver::New { args, named, .. }, .. } => {
            assert!(args.is_empty() && named.is_empty());
        }
        other => panic!("expected a new receiver, got {other:?}"),
    }
}

#[test]
fn the_arguments_lower_by_the_same_rules_as_any_other_call() {
    match lowered("$b->m(1, $y, g(2), n: 's')") {
        ArgValue::MethodCall { args, named, .. } => {
            assert_eq!(args[0], ArgValue::Int(1));
            assert_eq!(args[1], ArgValue::Var("y".into()));
            assert!(matches!(&args[2], ArgValue::Call(n, a) if n == "g" && a.len() == 1));
            assert_eq!(named.len(), 1);
            assert_eq!(named[0].value, ArgValue::Str("s".into()));
        }
        other => panic!("lowered to {other:?}"),
    }
    // Nesting is the point: a method call is an argument of a method call.
    match lowered("$b->m($c->n())") {
        ArgValue::MethodCall { args, .. } => {
            assert!(matches!(&args[0], ArgValue::MethodCall { .. }));
        }
        other => panic!("lowered to {other:?}"),
    }
}

#[test]
fn the_spellings_the_carrier_cannot_say_still_widen_to_other() {
    for src in [
        // A dynamic method name, in both spellings.
        "$b->$m()",
        "$b::$m()",
        // A receiver no `Receiver` names: a chain deeper than one hop, an offset
        // read, a call result.
        "$a->b->c->m()",
        "$a[0]->m()",
        "g()->m()",
        // A variable class portion.
        "$c::m()",
        // A spread argument list: the positional prefix is not the written call.
        "$b->m(...$args)",
        "Foo::m(...$args)",
        // A first-class callable is a value, not a call (see `closures.rs`).
        "$b->m(...)",
        "Foo::m(...)",
    ] {
        assert_eq!(lowered(src), ArgValue::Other, "`{src}` must not be carried");
    }
}

#[test]
fn the_carrier_is_neither_literal_nor_self_evident() {
    let v = lowered("$b->m()");
    assert!(!v.is_literal());
    assert!(!v.is_concrete_value());
    // Nor does it make an array holding it self-evident (issue #39's rule).
    let arr = lowered("[$b->m(), 2]");
    assert!(!arr.is_concrete_value());
}

#[test]
fn render_spells_the_call_as_it_was_written() {
    for (src, want) in [
        ("$b->m()", "$b->m()"),
        ("$b?->m()", "$b?->m()"),
        ("$this->m()", "$this->m()"),
        ("$a->p->m()", "$a->p->m()"),
        ("(new C(1))->m()", "(new C())->m()"),
        ("Foo::m()", "Foo::m()"),
        ("self::m()", "self::m()"),
    ] {
        assert_eq!(lowered(src).render(), want, "render of `{src}`");
    }
}

#[test]
fn the_hash_excludes_the_spans_a_nested_arm_carries() {
    // The `Ternary`/`Coalesce` precedent, reached through the new arm: two calls
    // written at different offsets denote the same value and must hash alike, or
    // the binding memo would miss (ADR-0075 §2.1).
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let key = |v: &ArgValue| {
        let mut h = DefaultHasher::new();
        v.hash(&mut h);
        h.finish()
    };
    let a = lowered("$b->m($c ? 1 : 2)");
    let b = {
        let src = "<?php\n$pad = 1;\n$x = $b->m($c ? 1 : 2);\n";
        let tree = SourceTree::parse(src);
        tree.scopes()
            .iter()
            .find_map(|s| {
                s.stmts.iter().find_map(|st| match &st.kind {
                    StmtKind::Assign { var, value, .. } if var == "x" => Some(value.clone()),
                    _ => None,
                })
            })
            .expect("an assignment")
    };
    assert_ne!(a, b, "the arm spans differ, so `PartialEq` separates them");
    assert_eq!(key(&a), key(&b), "the hash is the denotation only");
}

/// A `NameRef` is only meaningful once resolved; this pins that the receiver's
/// class reference survives the widening with its qualification intact.
#[test]
fn the_receiver_news_class_reference_keeps_its_qualification() {
    let raw: NameRef = match callee("(new \\Ns\\C())->m()") {
        Callee::Method { receiver: Receiver::New { class, .. }, .. } => class,
        other => panic!("expected a new receiver, got {other:?}"),
    };
    assert_eq!(raw.simple(), "C");
}
