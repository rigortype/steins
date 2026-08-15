//! Folding integration at the inference layer, via a deterministic mock [`Folder`]
//! (no PHP) — proves the gate and provenance. Live-sidecar folding is CLI-tested.

use std::cell::RefCell;
use std::rc::Rc;

use steins_infer::{Diagnostic, Folder, NoFold, check, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// A canned folder mimicking the allowlisted builtins the tests use.
struct Mock;

impl Folder for Mock {
    fn fold(&mut self, name: &str, args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        match (name, args) {
            ("strtolower", [ArgValue::Str(s)]) => Some(ArgValue::Str(s.as_str()?.to_lowercase().into())),
            ("strtoupper", [ArgValue::Str(s)]) => Some(ArgValue::Str(s.as_str()?.to_uppercase().into())),
            ("strval", [ArgValue::Int(i)]) => Some(ArgValue::Str(i.to_string().into())),
            // `count` over a literal array (issue #39) — proves the GATE, not the fold.
            ("count", [ArgValue::Array(items)]) => {
                Some(ArgValue::Str(format!("n{}", items.len()).into()))
            }
            _ => None,
        }
    }
}

/// One call the gate forwarded to the folder.
type Ask = (String, Vec<ArgValue>);

/// Records every `(name, args)` the gate hands it and never folds (asserts on *asked*).
#[derive(Clone, Default)]
struct Spy(Rc<RefCell<Vec<Ask>>>);

impl Folder for Spy {
    fn fold(&mut self, name: &str, args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        self.0.borrow_mut().push((name.to_owned(), args.to_vec()));
        None
    }
}

/// The calls the gate forwarded to the folder for `src`.
fn asked(src: &str) -> Vec<Ask> {
    let spy = Spy::default();
    let mut folder = spy.clone();
    let _ = find(src, &mut folder);
    spy.0.borrow().clone()
}

/// A PHP source with an `n`-element literal array argument to `count`.
fn count_of_n(n: usize) -> String {
    let elems: Vec<String> = (0..n).map(|i| i.to_string()).collect();
    format!("{COERCIVE_INT}width(count([{}]));", elems.join(", "))
}

fn find(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check_with(&tree, &functions, "test.php", folder)
}

const COERCIVE_INT: &str = "<?php function width(int $w): int { return $w; }\n";
const STRICT_INT: &str =
    "<?php\ndeclare(strict_types=1);\nfunction width(int $w): int { return $w; }\n";

#[test]
fn folds_builtin_in_argument_position() {
    let f = find(&format!("{COERCIVE_INT}width(strtolower(\"ABC\"));"), &mut Mock);
    assert_eq!(f.len(), 1);
    assert_eq!(
        f[0].message,
        "argument \"abc\" (folded from strtolower(\"ABC\")) to width() cannot become int $w — proven TypeError (coercive mode)"
    );
}

#[test]
fn folds_builtin_on_assignment_rhs() {
    let f = find(&format!("{COERCIVE_INT}$w = strtoupper(\"xy\");\nwidth($w);"), &mut Mock);
    assert_eq!(f.len(), 1, "got {f:#?}");
    // Chained through a variable: provenance is the immediate $w hop.
    assert!(f[0].message.contains("from $w, assigned at line"), "{}", f[0].message);
    assert!(f[0].message.contains("argument \"XY\""));
}

#[test]
fn non_literal_inner_arg_is_silent() {
    // A variable inner arg (not a literal) means the gate never asks the folder.
    let src = format!("{COERCIVE_INT}$x = $_GET['x'];\nwidth(strtolower($x));");
    assert_eq!(find(&src, &mut Mock).len(), 0);
}

#[test]
fn strval_folds_strict_flagged_coercive_silent() {
    // strval(5) → "5": strict flags string→int; coercive accepts a numeric string.
    let strict = find(&format!("{STRICT_INT}width(strval(5));"), &mut Mock);
    assert_eq!(strict.len(), 1, "strict flags string→int: {strict:#?}");
    assert!(strict[0].message.contains("(folded from strval(5))"));
    assert!(strict[0].message.contains("(strict mode)"));

    let coercive = find(&format!("{COERCIVE_INT}width(strval(5));"), &mut Mock);
    assert_eq!(coercive.len(), 0, "coercive coerces numeric string: {coercive:#?}");
}

#[test]
fn nofold_is_silent_for_folded_findings() {
    // The sound subset never executes the fold, so a folded-only finding vanishes.
    let src = format!("{COERCIVE_INT}width(strtolower(\"ABC\"));");
    assert_eq!(find(&src, &mut NoFold).len(), 0, "NoFold widens the fold");
    // But `check` (== NoFold) still reports direct literals.
    let direct = "<?php function width(int $w): int { return $w; }\nwidth(\"abc\");";
    let tree = SourceTree::parse(direct);
    let funcs = tree.functions().to_vec();
    assert_eq!(check(&tree, &funcs, "d.php").len(), 1);
}

// Array-literal fold arguments (issue #39): assert the GATE for `count`, `in_array`,
// `implode` on the `foldable` allowlist.

#[test]
fn a_literal_array_argument_reaches_the_folder() {
    let calls = asked(&format!("{COERCIVE_INT}width(count([1, 2, 3]));"));
    assert!(!calls.is_empty(), "the array arg passed the gate");
    assert!(
        calls.iter().all(|(n, a)| n == "count"
            && matches!(&a[..], [ArgValue::Array(items)] if items.len() == 3)),
        "got {calls:#?}"
    );

    let f = find(&format!("{COERCIVE_INT}width(count([1, 2, 3]));"), &mut Mock);
    assert_eq!(f.len(), 1, "folded array arg produces a finding, got {f:#?}");
    assert!(f[0].message.contains("folded from count([1, 2, 3])"), "{}", f[0].message);
}

#[test]
fn a_nested_literal_array_reaches_the_folder() {
    // Nesting is REPRESENTED, not widened — the whole tree crosses the seam.
    let calls = asked(&format!("{COERCIVE_INT}width(count([[1, 2], ['k' => 3]]));"));
    assert!(!calls.is_empty(), "nested array arg passed the gate");
    let [ArgValue::Array(outer)] = &calls[0].1[..] else { panic!("expected one array arg") };
    assert!(matches!(&outer[0].1, ArgValue::Array(inner) if inner.len() == 2));
}

#[test]
fn an_array_with_a_non_literal_element_never_reaches_the_folder() {
    // `$x` may hold anything (even an array), so the literal's length is not the
    // array's length — at top level, at depth, and a spread (never lowered to `Array`).
    assert!(asked(&format!("{COERCIVE_INT}width(count([1, $x]));")).is_empty());
    assert!(asked(&format!("{COERCIVE_INT}width(count([[1, [$x]]]));")).is_empty());
    assert!(asked(&format!("{COERCIVE_INT}width(count([1, ...$rest]));")).is_empty());
}

#[test]
fn a_provable_element_is_resolved_before_the_gate_judges_the_array() {
    // ADR-0062 S7: the gate judges each argument as the value it PROVABLY is, so a
    // foldable-call element is resolved first (`['a']`, one entry — an honest length).
    let calls = asked(&format!("{COERCIVE_INT}width(count([strtolower('A')]));"));
    assert!(!calls.is_empty(), "the resolved element let the array through");
    // strtolower is asked first; the Spy never folds, so count's arg stays unresolved.
    assert_eq!(calls[0].0, "strtolower");
    let f = find(&format!("{COERCIVE_INT}width(count([strtolower('A')]));"), &mut Mock);
    assert_eq!(f.len(), 1, "the resolved array folds, got {f:#?}");
    assert!(f[0].message.contains("folded from count(['a'])"), "{}", f[0].message);
}

#[test]
fn a_non_literal_key_never_reaches_the_folder() {
    // A key the analyzer cannot spell is CARRIED at lowering (issue #336), but still
    // not a fold argument — the key might collide with the written one.
    assert!(asked(&format!("{COERCIVE_INT}width(count([$k => 1, 'a' => 2]));")).is_empty());
    assert!(asked(&format!("{COERCIVE_INT}width(count([[$k => 1]]));")).is_empty());
}

#[test]
fn a_literal_array_folds_in_a_poisoned_scope_too() {
    // Poisoning (ADR-0027) makes every *variable* unknown, but a literal reads none.
    let src = format!(
        "{COERCIVE_INT}function poisoned() {{ extract($GLOBALS); width(count([1, 2, 3])); }}"
    );
    let calls = asked(&src);
    assert!(!calls.is_empty(), "a literal needs no env, so poisoning does not gate it");
    assert!(matches!(&calls[0].1[..], [ArgValue::Array(items)] if items.len() == 3));
    // But an element read out of that scope's env stays unknown, poisoned or not.
    let via_var = format!(
        "{COERCIVE_INT}function poisoned() {{ extract($GLOBALS); width(count([1, $x])); }}"
    );
    assert!(asked(&via_var).is_empty());
}

#[test]
fn the_empty_array_is_a_value_and_folds() {
    let calls = asked(&format!("{COERCIVE_INT}width(count([]));"));
    assert!(!calls.is_empty(), "count([]) is a fold, not a widen");
    assert!(matches!(&calls[0].1[..], [ArgValue::Array(items)] if items.is_empty()));
}

#[test]
fn an_oversized_literal_array_widens_instead_of_folding() {
    // The fold budget (256 entries, counted recursively) keeps big lookups off the IPC seam.
    assert!(!asked(&count_of_n(256)).is_empty(), "256 entries is inside the budget");
    assert!(asked(&count_of_n(257)).is_empty(), "257 entries widens");
}

#[test]
fn an_overdeep_literal_array_widens_instead_of_folding() {
    // The depth bound (8) keeps the recursive encoders off an unbounded stack.
    let nest = |d: usize| format!("{COERCIVE_INT}width(count({}1{}));", "[".repeat(d), "]".repeat(d));
    assert!(!asked(&nest(8)).is_empty(), "depth 8 is inside the budget");
    assert!(asked(&nest(9)).is_empty(), "depth 9 widens");
}

#[test]
fn user_function_named_like_builtin_is_not_folded() {
    // A same-file user function shadowing an allowlisted name must not reach the sidecar.
    let src = "<?php\nfunction width(int $w): int { return $w; }\nfunction strtolower(string $s): string { $x = 1; return $s; }\nwidth(strtolower(\"ABC\"));";
    assert_eq!(find(src, &mut Mock).len(), 0, "user fn is not folded");
}
