//! Issue #128 — closure return lane: native return checking + T0 summary rebind
//! for `$fn(...)` invocations.
//!
//! `ScopeOwner::Closure` answers `scope_return` from `Scope::ret_ty`, and a
//! proven-closure `$fn(args)` rebinds its summary like a free function. Capture
//! snapshots and binding descent follow ADR-0033.

use steins_infer::{
    DEBUG_PHPDOC_TYPE_ID, DEBUG_TYPE_ID, Diagnostic, Folder, ID as ARG_MISMATCH_ID, RETURN_ID,
    RETURN_MISMATCH_ID, check, check_with,
};
use steins_syntax::{ArgValue, SourceTree};

struct Mock;

impl Folder for Mock {
    fn fold(&mut self, name: &str, args: &[ArgValue]) -> Option<ArgValue> {
        match (name, args) {
            ("strtoupper", [ArgValue::Str(s)]) => Some(ArgValue::Str(s.to_uppercase())),
            ("str_repeat", [ArgValue::Str(s), ArgValue::Int(n)]) => {
                Some(ArgValue::Str(s.repeat(usize::try_from(*n).ok()?)))
            }
            _ => None,
        }
    }
}

fn findings(src: &str, folder: Option<&mut dyn Folder>) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    match folder {
        Some(f) => check_with(&tree, &functions, "test.php", f),
        None => check(&tree, &functions, "test.php"),
    }
}

fn one_type(src: &str) -> String {
    let ds: Vec<_> = findings(src, None)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .collect();
    assert_eq!(ds.len(), 1, "expected one debug.type, got {ds:?}");
    ds[0].message.replace("dumped type: ", "")
}

fn one_folded(src: &str) -> String {
    let ds: Vec<_> = findings(src, Some(&mut Mock))
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .collect();
    assert_eq!(ds.len(), 1, "expected one debug.type, got {ds:?}");
    ds[0].message.replace("dumped type: ", "")
}

fn count(src: &str, id: &str) -> usize {
    findings(src, None).iter().filter(|d| d.id == id).count()
}

fn one_phpdoc(src: &str) -> String {
    let ds: Vec<_> = findings(src, None)
        .into_iter()
        .filter(|d| d.id == DEBUG_PHPDOC_TYPE_ID)
        .collect();
    assert_eq!(ds.len(), 1, "expected one debug.phpdoc-type, got {ds:?}");
    ds[0].message.replace("dumped phpdoc type: ", "")
}

// (a) Conformance — closure return sites against native `: R`

#[test]
fn closure_native_return_mismatch_fires() {
    // Arrow body is a `return` of the expression; `: int` vs `"hi"` is a proven
    // TypeError at the closure definition scope.
    let src = "<?php\n\
        $f = fn(): int => \"hi\";\n";
    assert_eq!(count(src, RETURN_ID), 1, "native return mismatch on the arrow body");
}

#[test]
fn closure_native_return_good_is_silent() {
    let src = "<?php\n\
        $f = fn(): int => 42;\n";
    assert_eq!(count(src, RETURN_ID), 0);
}

#[test]
fn block_closure_return_mismatch_fires() {
    let src = "<?php\n\
        $f = function (): int {\n\
            return \"hi\";\n\
        };\n";
    assert_eq!(count(src, RETURN_ID), 1);
}

#[test]
fn generator_closure_return_is_not_checked_against_generator() {
    // `: Generator` names the *call* result. In-body `return 7` is getReturn()'s
    // value and must not fire type.return-mismatch (issue #128 review).
    let src = "<?php\n\
        $f = function (): Generator {\n\
            yield 1;\n\
            return 7;\n\
        };\n";
    assert_eq!(count(src, RETURN_ID), 0, "generator body return is not a Generator value");
}

#[test]
fn generator_function_return_is_not_checked_against_generator() {
    let src = "<?php\n\
        function gen(): Generator {\n\
            yield 1;\n\
            return 7;\n\
        }\n";
    assert_eq!(count(src, RETURN_ID), 0);
}

// (b) Value lane — `$fn(...)` summary rebinds at assignment

#[test]
fn closure_call_summary_rebinds_literal() {
    let src = "<?php\n\
        $f = fn(int $x): int => $x;\n\
        $y = $f(42);\n\
        \\PHPStan\\dumpType($y);\n";
    assert_eq!(one_type(src), "42");
}

#[test]
fn flagship_closure_greet_inlines_to_its_value() {
    // Method/function twin of the greet flagship, as a locally bound closure.
    let src = "<?php\n\
        $greet = fn(int $times, string $name): string =>\n\
            str_repeat(\"Hello, \" . $name . \"! \", $times);\n\
        $x = $greet(2, \"World\");\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_folded(src), "'Hello, World! Hello, World! '");
}

#[test]
fn closure_call_factless_falls_to_declared_floor() {
    let src = "<?php\n\
        $f = fn(int $x): int => rand();\n\
        $y = $f(1);\n\
        \\PHPStan\\dumpType($y);\n";
    assert_eq!(one_type(src), "int");
}

#[test]
fn named_arg_closure_call_keeps_declared_floor() {
    // Named args refuse binding descent (positional map), but the declared
    // return arms must still seed the floor — same rung as free functions.
    let src = "<?php\n\
        $f = fn(int $x): int => rand();\n\
        $y = $f(x: 1);\n\
        \\PHPStan\\dumpType($y);\n";
    assert_eq!(one_type(src), "int");
}

#[test]
fn first_class_callable_summary_rebinds() {
    // `$fn = pick(...); $fn(1)` resolves as a named free function through the
    // proven ClosureVal::Named path.
    let src = "<?php\n\
        function pick(int $x): int { return 42; }\n\
        $fn = pick(...);\n\
        $y = $fn(1);\n\
        \\PHPStan\\dumpType($y);\n";
    assert_eq!(one_type(src), "42");
}

#[test]
fn string_callable_summary_rebinds() {
    let src = "<?php\n\
        function pick(int $x): int { return 7; }\n\
        $fn = \"pick\";\n\
        $y = $fn(1);\n\
        \\PHPStan\\dumpType($y);\n";
    assert_eq!(one_type(src), "7");
}

#[test]
fn named_arg_string_callable_keeps_declared_floor() {
    // A string-callable with named/spread args keeps the declared return floor,
    // like local closures and first-class callables — not falling to `unknown`.
    let src = "<?php\n\
        function pick(int $x): int { return rand(); }\n\
        $fn = 'pick';\n\
        $y = $fn(x: 1);\n\
        \\PHPStan\\dumpType($y);\n";
    assert_eq!(one_type(src), "int");
}

#[test]
fn named_arg_first_class_callable_keeps_declared_floor() {
    let src = "<?php\n\
        function pick(int $x): int { return rand(); }\n\
        $fn = pick(...);\n\
        $y = $fn(x: 1);\n\
        \\PHPStan\\dumpType($y);\n";
    assert_eq!(one_type(src), "int");
}

// Capture stratum — Asserted must not launder through the summary (issue #128)

#[test]
fn asserted_capture_summary_stays_asserted() {
    // Regression: a capture snapshot must preserve stratum. If it re-seeds as
    // Verified, `$result = $f()` launders an Asserted 'hi' into a proof premise.
    let src = "<?php\n\
        /** @phpstan-assert 'hi' $v */\n\
        function claimHi($v): void {}\n\
        function takesInt(int $n): void {}\n\
        $x = (string) rand();\n\
        claimHi($x);\n\
        $f = function () use ($x): string {\n\
            return $x;\n\
        };\n\
        $result = $f();\n\
        \\PHPStan\\dumpType($result);\n\
        takesInt($result);\n";
    assert_eq!(one_type(src), "'hi' (asserted)");
    assert_eq!(
        count(src, ARG_MISMATCH_ID),
        0,
        "Asserted capture summary must not premise type.argument-mismatch"
    );
}

#[test]
fn memo_does_not_launder_asserted_after_verified_same_value() {
    // Issue #128 review: BindingKey without stratum collides Verified `$f('hi')`
    // with Asserted `$f($u)` (same Singleton value) inside one outer descent —
    // memo replay would hand a Verified summary to the Asserted call and premise
    // a proof finding. Stratum is part of the key so the second call re-walks.
    let src = "<?php\n\
        /** @phpstan-assert 'hi' $v */\n\
        function claimHi($v): void {}\n\
        function takesInt(int $n): void {}\n\
        function outer(int $trigger, $u): string {\n\
            $f = fn(string $x): string => $x;\n\
            $a = $f('hi');\n\
            claimHi($u);\n\
            $b = $f($u);\n\
            return $b;\n\
        }\n\
        $result = outer(1, (string) rand());\n\
        \\PHPStan\\dumpType($result);\n\
        takesInt($result);\n";
    assert_eq!(one_type(src), "'hi' (asserted)");
    assert_eq!(
        count(src, ARG_MISMATCH_ID),
        0,
        "Verified memo must not launder Asserted same-value call into a proof premise"
    );
}

#[test]
fn memo_does_not_launder_asserted_capture_after_verified_same_value() {
    // The parameter-stratum fixture above does not by itself prove that
    // `use:{name}` entries carry stratum too. Two walks of `wrap` share one memo
    // and instantiate the same closure definition with equal capture values but
    // different trust; omitting capture stratum would replay the first summary.
    let src = "<?php\n\
        /** @phpstan-assert 'hi' $v */\n\
        function claimHi($v): void {}\n\
        function takesInt(int $n): void {}\n\
        function wrap(string $x): string {\n\
            $f = function () use ($x): string { return $x; };\n\
            return $f();\n\
        }\n\
        function outer(int $trigger, $u): string {\n\
            $a = wrap('hi');\n\
            claimHi($u);\n\
            $b = wrap($u);\n\
            return $b;\n\
        }\n\
        $result = outer(1, (string) rand());\n\
        \\PHPStan\\dumpType($result);\n\
        takesInt($result);\n";
    assert_eq!(one_type(src), "'hi' (asserted)");
    assert_eq!(
        count(src, ARG_MISMATCH_ID),
        0,
        "Verified capture memo must not launder an Asserted same-value capture"
    );
}

// (c) Phpdoc `@return` — docblock adoption (issue #128, second leg). Two
// spellings share one grammar (ADR-0029 whitespace gap): inline before the
// closure expression's first token wins; the enclosing statement's docblock is
// adopted only when the statement is a simple `$f = <closure>;` assignment.

#[test]
fn inline_phpdoc_return_mismatch_fires() {
    let src = "<?php\n\
        $f = /** @return string */ function () {\n\
            return 42;\n\
        };\n";
    assert_eq!(count(src, RETURN_MISMATCH_ID), 1, "inline docblock adopts");
}

#[test]
fn statement_phpdoc_return_mismatch_fires() {
    let src = "<?php\n\
        /** @return string */\n\
        $f = function () {\n\
            return 42;\n\
        };\n";
    assert_eq!(count(src, RETURN_MISMATCH_ID), 1, "statement docblock adopts");
}

#[test]
fn phpdoc_return_match_is_silent() {
    let src = "<?php\n\
        /** @return string */\n\
        $f = function () {\n\
            return \"hi\";\n\
        };\n";
    assert_eq!(count(src, RETURN_MISMATCH_ID), 0);
}

#[test]
fn arrow_fn_statement_phpdoc_return_mismatch_fires() {
    let src = "<?php\n\
        /** @return string */\n\
        $f = fn() => 42;\n";
    assert_eq!(count(src, RETURN_MISMATCH_ID), 1);
}

#[test]
fn static_closure_inline_phpdoc_adjacency_is_to_the_static_keyword() {
    // The closure expression's first token is `static`, not `function` — the
    // inline position must still adopt across it.
    let src = "<?php\n\
        $f = /** @return string */ static function () {\n\
            return 42;\n\
        };\n";
    assert_eq!(count(src, RETURN_MISMATCH_ID), 1);
}

#[test]
fn generator_closure_phpdoc_return_is_not_checked() {
    // The native leg's generator skip holds for the phpdoc lane identically: a
    // `yield` body's in-body `return` is `Generator::getReturn()`'s value, so
    // even a blatant `@return int` mismatch stays silent.
    let src = "<?php\n\
        /** @return int */\n\
        $f = function () {\n\
            yield 1;\n\
            return \"hi\";\n\
        };\n";
    assert_eq!(count(src, RETURN_MISMATCH_ID), 0, "generator scopes skip the phpdoc check");
}

#[test]
fn embedded_closure_does_not_adopt_the_statement_docblock() {
    // A closure in a call-argument position is not the statement's whole RHS —
    // the statement docblock stays with the statement, in both the bare-call
    // and assigned-call spellings.
    let bare = "<?php\n\
        /** @return string */\n\
        array_map(function () {\n\
            return 42;\n\
        }, []);\n";
    let assigned = "<?php\n\
        /** @return string */\n\
        $r = array_map(function () {\n\
            return 42;\n\
        }, []);\n";
    assert_eq!(count(bare, RETURN_MISMATCH_ID), 0);
    assert_eq!(count(assigned, RETURN_MISMATCH_ID), 0);
}

#[test]
fn inline_docblock_beats_the_statement_docblock() {
    // Both positions present: inline wins. `@return string` (inline) is violated
    // by `return 42` even though the statement-level `@return int` would accept
    // it — and the mirror image stays silent where the statement-level claim
    // would have fired.
    let inline_fires = "<?php\n\
        /** @return int */\n\
        $f = /** @return string */ function () {\n\
            return 42;\n\
        };\n";
    let inline_accepts = "<?php\n\
        /** @return int */\n\
        $f = /** @return string */ function () {\n\
            return \"hi\";\n\
        };\n";
    assert_eq!(count(inline_fires, RETURN_MISMATCH_ID), 1);
    assert_eq!(count(inline_accepts, RETURN_MISMATCH_ID), 0);
}

#[test]
fn var_only_statement_docblock_leaves_the_closure_unchecked() {
    // ADR-0073's `@var` cast lane and this `@return` lane read different tags
    // from the same statement docblock; a `@var`-only docblock carries no
    // `@return` and so checks nothing on the closure body.
    let src = "<?php\n\
        /** @var \\Closure $f */\n\
        $f = function () {\n\
            return 42;\n\
        };\n";
    assert_eq!(count(src, RETURN_MISMATCH_ID), 0);
}

#[test]
fn non_doc_comment_breaks_the_adoption_adjacency() {
    // The shared grammar: a blank line still adopts; an intervening non-doc
    // comment silences (exactly `stmt_docblock`'s discipline).
    let blank_line_adopts = "<?php\n\
        /** @return string */\n\
        \n\
        $f = function () {\n\
            return 42;\n\
        };\n";
    let comment_silences = "<?php\n\
        /** @return string */\n\
        // a note\n\
        $f = function () {\n\
            return 42;\n\
        };\n";
    assert_eq!(count(blank_line_adopts, RETURN_MISMATCH_ID), 1);
    assert_eq!(count(comment_silences, RETURN_MISMATCH_ID), 0);
}

#[test]
fn phpdoc_refines_the_declared_floor_at_a_refused_call() {
    // The value-lane seam (issue #128 second leg): a named-arg call refuses
    // binding descent, and the floor it keeps composes native `: int` with the
    // adopted `@return positive-int` through the same `refine_contract_arms`
    // precedence free functions use — an Asserted refinement within the
    // Verified native envelope.
    let src = "<?php\n\
        /** @return positive-int */\n\
        $f = function (int $x): int {\n\
            return rand();\n\
        };\n\
        $y = $f(x: 1);\n\
        \\PHPStan\\dumpPhpDocType($y);\n";
    assert_eq!(one_phpdoc(src), "int<1, max> (asserted)");
}
