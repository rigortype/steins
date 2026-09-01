//! Argument-unpacking lowering: which spreads name their own arguments (#616).
//!
//! A spread of an array **literal** flattens into the positional list at its
//! written position — its cardinality, order and values are all written in the
//! source, so the flattened list is the call that was written. Every other
//! spread keeps the refusal it always had: a prefix read as the whole list would
//! answer about a different call.
//!
//! # Behavioral witnesses at `PINNED_PHP` (8.5.9, `php -r`)
//!
//! ```text
//! f(1, ...[2, 3])           => [1, 2, 3]            // flattens positionally
//! f(...[2 => 'a', 0 => 'b'])=> [0 => 'a', 1 => 'b'] // int keys DISCARDED, iteration order
//! f(...[1 => 'a', 2 => 'b'])=> [0 => 'a', 1 => 'b'] // non-contiguous, same
//! f(...["1" => 1])          => [0 => 1]             // key normalization first
//! f(...[true => 1])         => [0 => 1]             // ditto
//! f(...[])                  => []                   // contributes nothing
//! named(...['y' => 2])      => 'x=dx y=2 z=dz'      // STRING key = NAMED argument (8.1+)
//! named(...['y'=>2,'x'=>1]) => 'x=1 y=2 z=dz'       // ...bound by name, not position
//! f(...['a' => 1, 0 => 2])  => Error: Cannot use positional argument
//!                                    after named argument during unpacking
//! f(...[2, 3], 4)           => Fatal: Cannot use positional argument
//!                                    after argument unpacking     (COMPILE error)
//! f(1, ...[2, 3], ...[4])   => [1, 2, 3, 4]         // several spreads compose
//! ```
//!
//! The string-key row is why a string-keyed literal is **declined** rather than
//! flattened: it is a named argument, and `ArgValue::Call` has no named slot.

use steins_syntax::{ArgValue, ArrayKey, CallExpr, SourceTree, StmtKind, flatten_spread_operand};

/// The value assigned to `$x` by the first assignment — the **value** lane.
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

/// The first call statement's `CallExpr` — the **statement** lane.
fn call_stmt(src: &str) -> CallExpr {
    let tree = SourceTree::parse(&format!("<?php\n{src};\n"));
    tree.calls().first().expect("a call").clone()
}

/// The positional argument values of the first call statement.
fn stmt_args(src: &str) -> Vec<ArgValue> {
    call_stmt(src).args.into_iter().map(|a| a.value).collect()
}

// ---------------------------------------------------------------------------
// Criterion 1: a literal spread flattens at its written position, and the
// flattened list is compared POSITIONALLY against the written-out call.
// ---------------------------------------------------------------------------

#[test]
fn a_literal_spread_lowers_to_the_call_the_written_out_arguments_lower_to() {
    // The whole contract in one line: the two spellings are indistinguishable
    // after lowering, so every per-callee rule sees the same call.
    for (spread, written) in [
        ("f(1, ...[2, 3])", "f(1, 2, 3)"),
        ("f(...[1, 2, 3])", "f(1, 2, 3)"),
        ("f(...[1], ...[2, 3])", "f(1, 2, 3)"),
        ("f(1, ...[2], ...[3])", "f(1, 2, 3)"),
        // An empty literal contributes nothing at all.
        ("f(1, ...[])", "f(1)"),
        ("f(...[])", "f()"),
        // Nested literals stay nested — flattening is one level, as PHP's is.
        ("f(...[[17]])", "f([17])"),
        ("f([], ...[[17]])", "f([], [17])"),
        // Carriers flatten as themselves; the walk resolves them later.
        ("f(...[$a, $b])", "f($a, $b)"),
    ] {
        assert_eq!(lowered(spread), lowered(written), "value lane: `{spread}` vs `{written}`");
        assert_eq!(stmt_args(spread), stmt_args(written), "statement lane: `{spread}` vs `{written}`");
    }
}

#[test]
fn the_flattened_call_keeps_its_callee_and_carrier_form() {
    match lowered("array_merge([], ...[[17]])") {
        ArgValue::Call(name, args) => {
            assert_eq!(name, "array_merge");
            assert_eq!(args, vec![ArgValue::Array(vec![]), ArgValue::Array(vec![(ArrayKey::Auto, ArgValue::Int(17))])]);
        }
        other => panic!("lowered to {other:?}"),
    }
    // A method call in value position takes the same route (issue #386 carrier).
    assert!(matches!(lowered("$o->m(1, ...[2, 3])"), ArgValue::MethodCall { .. }));
    assert_eq!(lowered("$o->m(1, ...[2, 3])"), lowered("$o->m(1, 2, 3)"));
    assert_eq!(lowered("Foo::m(...[1, 2])"), lowered("Foo::m(1, 2)"));
}

// ---------------------------------------------------------------------------
// Criterion 2: a general spread still declines, and its positional prefix is
// never read as the whole call.
// ---------------------------------------------------------------------------

#[test]
fn a_general_spread_still_declines_the_whole_call() {
    for expr in [
        "f(...$args)",
        "f(1, ...$args)",
        "f(...g())",
        "f(1, ...g())",
        "f(...$o->p)",
        "f(...[1], ...$rest)",
        "$o->m(1, ...$args)",
        "Foo::m(...$args)",
    ] {
        assert_eq!(lowered(expr), ArgValue::Other, "`{expr}` must decline");
    }
}

#[test]
fn the_prefix_of_a_general_spread_is_never_the_call() {
    // The trap this refusal exists for: `f(1, ...$args)` is NOT `f(1)`.
    assert_ne!(lowered("f(1, ...$args)"), lowered("f(1)"));
    assert_ne!(lowered("min(0, ...$args)"), lowered("min(0)"));
    // In the statement lane the prefix is still recorded, but flagged unproven,
    // so no reader may mistake it for a complete argument list.
    let call = call_stmt("f(1, ...$args)");
    assert_eq!(call.args.len(), 1);
    assert!(call.has_spread);
    assert!(!call.positional_only);
}

// ---------------------------------------------------------------------------
// Criterion 3: string-keyed and non-contiguously-keyed literals, per the
// PINNED_PHP measurements in the module docs.
// ---------------------------------------------------------------------------

#[test]
fn integer_keys_are_discarded_and_the_values_flatten_in_iteration_order() {
    // Measured: `f(...[2 => 'a', 0 => 'b'])` → `['a', 'b']`. The written key is
    // not the argument position; the iteration order is.
    assert_eq!(stmt_args("f(...[2 => 1, 0 => 2])"), stmt_args("f(1, 2)"));
    assert_eq!(stmt_args("f(...[1 => 1, 2 => 2])"), stmt_args("f(1, 2)"));
    assert_eq!(stmt_args("f(...[5 => 1, 2])"), stmt_args("f(1, 2)"));
    // PHP key normalization runs first, so these are integer keys too.
    assert_eq!(stmt_args("f(...[true => 1])"), stmt_args("f(1)"));
    assert_eq!(stmt_args("f(...['1' => 1])"), stmt_args("f(1)"));
    // Last-wins on a duplicate key removes an argument — the flattened count is
    // the normalized array's, never the written element count.
    assert_eq!(stmt_args("f(...[0 => 1, 0 => 2])"), stmt_args("f(2)"));
    assert_eq!(stmt_args("f(...[5 => 1, 6 => 2, 5 => 3])"), stmt_args("f(3, 2)"));
}

#[test]
fn a_string_keyed_literal_declines_because_it_is_a_named_argument() {
    // Measured at PINNED_PHP: `named(...['y' => 2])` binds `$y`, NOT `$x`.
    // Flattening it positionally would bind the wrong parameter, and
    // `ArgValue::Call` has no named slot to put it in instead.
    for expr in [
        "f(...['a' => 1])",
        "f(...['a' => 1, 'b' => 2])",
        "f(1, ...['b' => 2])",
        // Mixed: PHP raises "Cannot use positional argument after named
        // argument during unpacking" at runtime; declining covers it.
        "f(...['a' => 1, 0 => 2])",
        "f(...[0 => 1, 'a' => 2])",
    ] {
        assert_eq!(lowered(expr), ArgValue::Other, "`{expr}` must decline");
        assert!(call_stmt(expr).has_spread, "`{expr}` must stay unproven");
    }
}

#[test]
fn a_literal_whose_positions_depend_on_the_php_minor_declines() {
    // `[-5 => 'a', 'b']` puts `'b'` at 0 before PHP 8.3 and at -4 from 8.3 —
    // and last-wins can then fold differently. A spread whose flattened list is
    // not the same under every supported minor is not one the source names.
    assert_eq!(lowered("f(...[-5 => 1, 2])"), ArgValue::Other);
    // A non-literal key is unresolvable under EVERY rule (issue #336).
    assert_eq!(lowered("f(...[$k => 1])"), ArgValue::Other);
}

// ---------------------------------------------------------------------------
// Criterion 4: named arguments keep their existing refusal, unchanged.
// ---------------------------------------------------------------------------

#[test]
fn named_arguments_keep_their_refusal() {
    // `ArgValue::Call` has no named slot at all, so a free function carrying a
    // named argument declines outright — including alongside a spread that
    // would otherwise have flattened.
    for expr in ["f(a: 1)", "f(1, b: 2)", "f(...[1], b: 2)", "f(a: 1, ...[2])"] {
        assert_eq!(lowered(expr), ArgValue::Other, "`{expr}` must decline");
    }
    // `ArgValue::MethodCall` DOES carry a `named` slot (issue #386) and always
    // has; it is the positional-only binding descent that declines on it. That
    // asymmetry is untouched here — only the spread half moved.
    match lowered("$o->m(a: 1)") {
        ArgValue::MethodCall { args, named, .. } => {
            assert!(args.is_empty());
            assert_eq!(named.len(), 1);
            assert_eq!(named[0].name, "a");
        }
        other => panic!("`$o->m(a: 1)` lowered to {other:?}"),
    }
    // The statement lane still separates them, and a named argument never
    // becomes a spread: `has_spread` stays down, `positional_only` goes down.
    let call = call_stmt("f(1, b: 2)");
    assert!(!call.has_spread);
    assert!(!call.positional_only);
    assert_eq!(call.named_args.len(), 1);
    // A literal spread AFTER a named argument does not flatten — its values
    // would land at positions the named argument already displaced.
    let call = call_stmt("f(a: 1, ...[2, 3])");
    assert!(call.has_spread);
    assert!(call.args.is_empty());
}

// ---------------------------------------------------------------------------
// Criterion 5: `has_spread` is pinned for BOTH outcomes. Six call sites read
// it; all of them ask the same question — "may I trust this argument count?"
// ---------------------------------------------------------------------------

#[test]
fn has_spread_is_down_exactly_when_the_argument_count_is_proven() {
    // Proven: no spread at all, or every spread a literal.
    for (src, argc) in [
        ("f(1, 2, 3)", 3),
        ("f(1, ...[2, 3])", 3),
        ("f(...[1, 2, 3])", 3),
        ("f(...[1], ...[2, 3])", 3),
        ("f(...[])", 0),
        ("f(1, ...[])", 1),
        ("f(...[2 => 1, 0 => 2])", 2),
    ] {
        let call = call_stmt(src);
        assert!(!call.has_spread, "`{src}` proves its count");
        assert!(call.positional_only, "`{src}` is positional only");
        assert_eq!(call.args.len(), argc, "`{src}` argument count");
    }
    // Unproven: a general spread anywhere, in any position.
    for src in [
        "f(...$args)",
        "f(1, ...$args)",
        "f(...g())",
        "f(...[1], ...$rest)",
        "f(...$rest, ...[1])",
        "f(...['a' => 1])",
        "f(...[-5 => 1, 2])",
    ] {
        let call = call_stmt(src);
        assert!(call.has_spread, "`{src}` cannot prove its count");
        assert!(!call.positional_only, "`{src}` is not positional only");
    }
}

#[test]
fn a_positional_after_any_unpacking_stays_unanalyzable() {
    // `f(...[2, 3], 4)` is a PHP COMPILE error ("Cannot use positional argument
    // after argument unpacking"), so the list is not a call shape to answer
    // about — flattening the spread must not make it look canonical.
    let call = call_stmt("f(...[2, 3], 4)");
    assert!(call.has_spread);
    assert_eq!(lowered("f(...[2, 3], 4)"), ArgValue::Other);
    // The general-spread form was already unanalyzable and stays so.
    assert!(call_stmt("f(...$args, 4)").has_spread);
}

// ---------------------------------------------------------------------------
// The helper on its own — the seam the array-literal slice will reuse.
// ---------------------------------------------------------------------------

#[test]
fn flatten_spread_operand_answers_only_for_a_literal_array() {
    let arr = |items: Vec<(ArrayKey, ArgValue)>| ArgValue::Array(items);
    assert_eq!(
        flatten_spread_operand(&arr(vec![
            (ArrayKey::Auto, ArgValue::Int(1)),
            (ArrayKey::Auto, ArgValue::Int(2)),
        ])),
        Some(vec![ArgValue::Int(1), ArgValue::Int(2)])
    );
    assert_eq!(flatten_spread_operand(&arr(vec![])), Some(vec![]));
    // Not an array literal: nothing else names its own cardinality.
    assert_eq!(flatten_spread_operand(&ArgValue::Var("a".to_owned())), None);
    assert_eq!(flatten_spread_operand(&ArgValue::Other), None);
    assert_eq!(flatten_spread_operand(&ArgValue::Int(1)), None);
    assert_eq!(flatten_spread_operand(&ArgValue::Call("f".to_owned(), vec![])), None);
    // A string key is a named argument, not a position.
    assert_eq!(
        flatten_spread_operand(&arr(vec![(ArrayKey::Str("a".into()), ArgValue::Int(1))])),
        None
    );
}
