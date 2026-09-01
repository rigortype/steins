//! `isset(…)` as a **value** (issue #579), the value-position twin of #414.
//!
//! Before this, `isset` in value position lowered to `ArgValue::Other` and
//! rendered `unknown` — not `bool`. Nothing declined it; `isset` is a construct,
//! not a call, so no seam was ever asked. Meanwhile `array_key_exists('p', $z)`
//! next to it answered `true` (issue #343).
//!
//! What this file pins, and deliberately nothing else:
//!
//! 1. **The rule is #343's, one step stronger.** `array_key_exists` asks whether
//!    the key is present; `isset` asks that AND that the value is provably
//!    non-null. The two conjoin, and the four-row table falls out.
//! 2. **The `bool` floor is total.** `isset` evaluates to a `bool` whatever it
//!    tests, so an *undecided* one is `bool` — a fact about the construct, not a
//!    guess about its operand. This is why an unmodelled operand renders `bool`
//!    rather than `unknown`.
//! 3. **The stratum split** (issue #260's ruling, ADR-0062 A-G9's corollary):
//!    `Maybe -> bool` is the construct's own guarantee, premised on no operand,
//!    so it is **Verified always**; `Yes -> true` / `No -> false` say **which**
//!    bool and rest on the subject's fact, so they carry its stratum. A verdict
//!    over a `@param array{…}` is `Asserted` and cannot premise a proof-layer
//!    finding; the same verdict over a witnessed literal is not.
//! 4. **The GUARD path is untouched.** `eval_cond` still answers `Maybe` for
//!    `CondExpr::Isset`/`IssetVar`, because deciding *reachability* from an
//!    Asserted shape would let a docblock silence the env-free pass.
//!
//! `empty(…)` is not this slice and still answers `unknown`.

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, check};
use steins_syntax::SourceTree;

/// Every `debug.type` body in `src`, in source order, on the pure `check` path.
fn types(src: &str) -> Vec<String> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let ds: Vec<Diagnostic> = check(&tree, &functions, "test.php");
    ds.into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect()
}

/// The dumps inside a function whose body is `body`, with `prelude` (a docblock
/// plus signature) written verbatim.
fn dumps(prelude: &str, body: &str) -> Vec<String> {
    types(&format!("<?php\n{prelude} {{\n{body}\n}}\n"))
}

/// A single dump of `<expr>` under the declared shape the four-row table is
/// stated over. Answers carry `(asserted)` — the shape came from a docblock.
fn declared(expr: &str) -> String {
    let prelude = "/** @param array{req: int, opt?: int, nul: null, nullable: ?int} $z */\n\
                   function f(array $z, int $n): void";
    let ds = dumps(prelude, &format!("    \\PHPStan\\dumpType({expr});"));
    assert_eq!(ds.len(), 1, "expected one dump for `{expr}`, got {ds:?}");
    ds.into_iter().next().expect("one dump")
}

/// The same question over a **witnessed** literal, which the walk proves rather
/// than reads out of a docblock — so the answers carry no `(asserted)`.
fn witnessed(expr: &str) -> String {
    let body = format!(
        "    $z = ['req' => 1, 'nul' => null];\n    \\PHPStan\\dumpType({expr});"
    );
    let ds = dumps("function g(int $n): void", &body);
    assert_eq!(ds.len(), 1, "expected one dump for `{expr}`, got {ds:?}");
    ds.into_iter().next().expect("one dump")
}

// (i) The witness from the issue: what used to be `unknown`.

#[test]
fn the_issue_witnesses_no_longer_answer_unknown() {
    let ds = dumps(
        "/** @param array{p: 1, q: string} $z */\nfunction f(array $z): void",
        "    \\PHPStan\\dumpType(isset($z['p']));\n\
         $b = isset($z['p']);\n\
         \\PHPStan\\dumpType($b);",
    );
    // The three positions the issue measured as `unknown`: the call argument,
    // the assignment, and the read of the assigned variable. (`array_key_exists`
    // is not cross-checked here — #343's rung needs the reflected signature the
    // bare `check` path carries no catalog for; the CLI probe in the PR body
    // shows the pair side by side.)
    assert_eq!(ds, vec!["true (asserted)"; 2]);
}

// (ii) The four-row table, over a DECLARED shape.

#[test]
fn a_required_field_with_a_non_null_value_is_true() {
    assert_eq!(declared("isset($z['req'])"), "true (asserted)");
}

/// The row that separates `isset` from `array_key_exists`: the key is there in
/// every realization the shape admits, and the value may still be `null`.
#[test]
fn a_required_field_with_a_nullable_value_is_bool() {
    assert_eq!(declared("isset($z['nullable'])"), "bool");
}

/// Not a row of the table, because `array_key_exists` has no such row — the same
/// conjunction produces it: present, and provably `null`.
#[test]
fn a_required_field_whose_value_is_null_is_false() {
    assert_eq!(declared("isset($z['nul'])"), "false (asserted)");
}

#[test]
fn an_undeclared_key_under_a_sealed_tail_is_false() {
    assert_eq!(declared("isset($z['nope'])"), "false (asserted)");
}

#[test]
fn an_optional_field_is_bool() {
    assert_eq!(declared("isset($z['opt'])"), "bool");
}

// (iii) The same table over a WITNESSED literal — and the stratum split.

#[test]
fn the_table_holds_over_a_witnessed_literal_without_the_asserted_marker() {
    assert_eq!(witnessed("isset($z['req'])"), "true");
    assert_eq!(witnessed("isset($z['nul'])"), "false");
    assert_eq!(witnessed("isset($z['nope'])"), "false");
}

/// PHP's own array-key cast, through the same `offset_key_of` primitive the read
/// and write sides use — so `$a[0]` and `$a["0"]` are one key here too, and a key
/// outside a proven array is absent whatever its spelling.
#[test]
fn the_key_cast_over_a_proven_array_is_phps_own() {
    let body = "    $z = [1, 2, 3];\n\
         \\PHPStan\\dumpType(isset($z[0]));\n\
         \\PHPStan\\dumpType(isset($z['0']));\n\
         \\PHPStan\\dumpType(isset($z[5]));\n\
         \\PHPStan\\dumpType(isset($z['string']));";
    assert_eq!(dumps("function f(): void", body), vec!["true", "true", "false", "false"]);
}

/// The stratum half of #260's ruling, pinned separately from the verdict half so
/// a later refactor cannot collapse them: the undecided answer is the
/// construct's own guarantee and is Verified even where the subject is not.
#[test]
fn the_undecided_answer_is_verified_even_over_an_asserted_subject() {
    // Never `bool (asserted)` — the `bool` is owed to PHP, not to the docblock.
    assert_eq!(declared("isset($z['opt'])"), "bool");
    assert_eq!(declared("isset($z['nullable'])"), "bool");
}

// (iv) The bare-variable operand.

#[test]
fn a_variable_the_walk_bound_to_a_non_null_value_is_true() {
    let ds = dumps(
        "function f(int $n): void",
        "    $s = 'x';\n\
         \\PHPStan\\dumpType(isset($s));\n\
         \\PHPStan\\dumpType(isset($n));",
    );
    // A native parameter type is the engine's own claim, so the binding and the
    // non-nullness are both the walk's.
    assert_eq!(ds, vec!["true", "true"]);
}

/// Sound with **no definedness premise at all**: bound-and-null and never-bound
/// both make `isset` false, so the two readings of the fact agree.
#[test]
fn a_variable_proven_null_is_false() {
    let ds = dumps(
        "function f(?string $s): void",
        "    $n = null;\n\
         \\PHPStan\\dumpType(isset($n));\n\
         \\PHPStan\\dumpType(isset($s));",
    );
    assert_eq!(ds, vec!["false", "bool"]);
}

/// The deferral, recorded rather than silent: PHP answers `false` here, and the
/// lowering's definedness lanes exclude an `isset` operand from the read sets by
/// construction — which is exactly what makes the guard silent — so the value
/// seam has no witness to read. The `bool` floor is what it gets instead of
/// `unknown`.
#[test]
fn a_never_bound_variable_defers_to_the_floor() {
    let ds = dumps("function f(): void", "    \\PHPStan\\dumpType(isset($nope));");
    assert_eq!(ds, vec!["bool"]);
}

// (v) Multi-argument `isset` is one conjunction, answered as one.

#[test]
fn a_multi_argument_isset_answers_as_a_conjunction() {
    assert_eq!(declared("isset($z['req'], $z['nul'])"), "false (asserted)");
    assert_eq!(declared("isset($z['req'], $z['opt'])"), "bool");
    assert_eq!(declared("isset($z['req'], $z['nope'])"), "false (asserted)");
}

/// One operand deciding `false` carries the whole expression even where its
/// siblings are unmodelled — Kleene conjunction, not a scan for unanimity.
#[test]
fn one_false_operand_decides_past_an_unmodelled_sibling() {
    assert_eq!(declared("isset($z['nope'], $z->prop)"), "false (asserted)");
    assert_eq!(declared("isset($z['req'], $z->prop)"), "bool");
}

// (vi) What stays at the floor, each for a stated reason.

#[test]
fn the_unmodelled_operands_answer_bool_not_unknown() {
    for expr in [
        // A property: the binding question is a declared-but-uninitialized one
        // the heap does not answer.
        "isset($z->prop)",
        // A path deeper than one offset.
        "isset($z['req']['deep'])",
        // A key the walk cannot prove.
        "isset($z[$n])",
    ] {
        assert_eq!(declared(expr), "bool", "for `{expr}`");
    }
}

/// `empty(…)` is a different question — `!isset(e) || !e` — and is not this
/// slice. Pinned so the boundary is a decision rather than an oversight.
#[test]
fn empty_is_not_answered_here() {
    assert_eq!(declared("empty($z['req'])"), "unknown");
    assert_eq!(declared("empty($z)"), "unknown");
}

// (vii) The guard path, untouched.

/// `isset` as a GUARD still decides nothing (ADR-0062 S4): the branch survives,
/// and the narrowing inside it is the payoff. If this ever starts pruning, the
/// value slice has leaked into reachability, which it must not.
#[test]
fn the_guard_path_still_decides_no_branch() {
    let ds = dumps(
        "/** @param array{req: int} $z */\nfunction f(array $z): void",
        "    if (isset($z['req'])) {\n\
         \\PHPStan\\dumpType($z['req']);\n\
         } else {\n\
         \\PHPStan\\dumpType($z['req']);\n\
         }",
    );
    // Both arms live: two dumps, neither dropped as dead code.
    assert_eq!(ds.len(), 2);
}

/// The `CondExpr::IssetVar` property issue #414 landed: a bare `isset($x)` guard
/// forgets nothing about `$x`, inside the branch and after it.
#[test]
fn a_bare_isset_guard_still_forgets_nothing() {
    let ds = dumps(
        "function f(): void",
        "    $s = 'x';\n\
         if (isset($s)) {\n\
         \\PHPStan\\dumpType($s);\n\
         }\n\
         \\PHPStan\\dumpType($s);",
    );
    assert_eq!(ds, vec!["'x'", "'x'"]);
}

// (viii) The value travels: one evaluator behind every seam.

#[test]
fn the_assignment_and_the_dump_of_one_expression_agree() {
    let ds = dumps(
        "/** @param array{req: int, opt?: int} $z */\nfunction f(array $z): void",
        "    \\PHPStan\\dumpType(isset($z['req']));\n\
         $a = isset($z['req']);\n\
         \\PHPStan\\dumpType($a);\n\
         \\PHPStan\\dumpType(isset($z['opt']));\n\
         $b = isset($z['opt']);\n\
         \\PHPStan\\dumpType($b);",
    );
    assert_eq!(ds, vec!["true (asserted)", "true (asserted)", "bool", "bool"]);
}

/// A returned `isset` crosses the exit as a fact, so a caller sees the verdict
/// rather than nothing.
#[test]
fn a_returned_isset_crosses_the_exit() {
    let ts = types(
        "<?php\n\
         /** @param array{req: int} $z */\n\
         function has(array $z): bool { return isset($z['req']); }\n\
         function caller(): void {\n\
         $z = ['req' => 1];\n\
         \\PHPStan\\dumpType(has($z));\n\
         }\n",
    );
    // `true`, not `true (asserted)`: the descent binds the callee's `$z` to the
    // CALLER's proven literal, so the verdict rests on the walk's own value
    // rather than on the callee's docblock.
    assert_eq!(ts, vec!["true"]);
}
