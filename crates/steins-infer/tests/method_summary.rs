//! ADR-0075 — a method/static call's return summary rebinds on the T0 rungs.
//!
//! The function leg is covered by `return_summary.rs` and `concat.rs`; this file pins the
//! method twin — a resolved method body's summary is consumed at `apply_assign` and return
//! composition exactly as a function's is. Value/argument-position method calls and
//! constructors are out of scope. Shared return-coverage soundness (opaque `may_return`,
//! untyped fallthrough) is regression-tested on both twins (Composer `findPackage`).

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, ID as ARG_MISMATCH_ID, check, check_with};
use steins_syntax::{ArgValue, SourceTree};

/// Canned folder for the two allowlisted builtins the greeter flagship needs.
struct Mock;

impl Folder for Mock {
    fn fold(&mut self, name: &str, args: &[ArgValue], _strict: bool) -> Option<ArgValue> {
        match (name, args) {
            ("strtoupper", [ArgValue::Str(s)]) => Some(ArgValue::Str(s.as_str()?.to_uppercase().into())),
            ("str_repeat", [ArgValue::Str(s), ArgValue::Int(n)]) => {
                Some(ArgValue::Str(s.as_str()?.repeat(usize::try_from(*n).ok()?).into()))
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

fn types(src: &str) -> Vec<String> {
    findings(src, None)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect()
}

fn one_type(src: &str) -> String {
    let ts = types(src);
    assert_eq!(ts.len(), 1, "expected exactly one debug.type dump, got {ts:?}");
    ts.into_iter().next().expect("one dump")
}

fn one_folded(src: &str) -> String {
    let ds: Vec<Diagnostic> = findings(src, Some(&mut Mock))
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .collect();
    assert_eq!(ds.len(), 1, "expected exactly one debug.type dump, got {ds:?}");
    ds[0].message.replace("dumped type: ", "")
}

fn count(src: &str, id: &str) -> usize {
    findings(src, None).iter().filter(|d| d.id == id).count()
}

// Flagship — the function twin across the receiver seam.

#[test]
fn flagship_method_greet_inlines_to_its_value() {
    // The method body walks, proves the string, and the summary rebinds at assignment (ADR-0075).
    let src = "<?php\n\
        final class Greeter {\n\
            public function greet(int $times, string $name): string {\n\
                return str_repeat(\"Hello, \" . $name . \"! \", $times);\n\
            }\n\
        }\n\
        $g = new Greeter();\n\
        $x = $g->greet(2, \"World\");\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_folded(src), "'Hello, World! Hello, World! '");
}

#[test]
fn flagship_static_greet_inlines_to_its_value() {
    let src = "<?php\n\
        final class Greeter {\n\
            public static function greet(int $times, string $name): string {\n\
                return str_repeat(\"Hello, \" . $name . \"! \", $times);\n\
            }\n\
        }\n\
        $x = Greeter::greet(2, \"World\");\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_folded(src), "'Hello, World! Hello, World! '");
}

#[test]
fn method_literal_return_agrees_with_function_twin() {
    // A one-arg method with a literal return: T0 produces `Singleton`, matching `return_summary.rs`.
    let via_method = "<?php\n\
        final class C {\n\
            public function pick(int $x): int { return 42; }\n\
        }\n\
        $a = (new C())->pick(1);\n\
        \\PHPStan\\dumpType($a);\n";
    let via_function = "<?php\n\
        function pick(int $x): int { return 42; }\n\
        $a = pick(1);\n\
        \\PHPStan\\dumpType($a);\n";
    assert_eq!(one_type(via_method), "42");
    assert_eq!(one_type(via_method), one_type(via_function), "method and function paths agree");
}

// Positive-int proof crosses the method boundary (return_summary flagship twin).

#[test]
fn method_positive_int_crosses_verified() {
    let src = "<?php\n\
        final class C {\n\
            public function f(int $trigger, int $n): int {\n\
                assert($n > 0);\n\
                return $n;\n\
            }\n\
        }\n\
        $x = (new C())->f(1, rand());\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "int<1, max>");
}

// Inheritance: the `this:` key must separate receivers inside ONE memo tree.

#[test]
fn inherited_body_does_not_replay_across_receivers_in_shared_memo() {
    // Top-level descents each mint a fresh memo, so `outer` forces both calls into one memo.
    // Without `this:` on the key, Sub1's summary would replay for Sub2 (`$x` would be `'A'`).
    let src = "<?php\n\
        class Base {\n\
            public function m(int $x): string {\n\
                return $this->tag($x);\n\
            }\n\
            public function tag(int $x): string { return \"?\"; }\n\
        }\n\
        final class Sub1 extends Base {\n\
            public function tag(int $x): string { return \"A\"; }\n\
        }\n\
        final class Sub2 extends Base {\n\
            public function tag(int $x): string { return \"B\"; }\n\
        }\n\
        function outer(int $trigger): string {\n\
            $a = (new Sub1())->m(1);\n\
            return (new Sub2())->m(1);\n\
        }\n\
        $x = outer(1);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "'B'");
}

// Silences and exact dispatch.

#[test]
fn exact_receiver_dispatches_inherited_override() {
    // `(new Sub())->call(1)` resolves `Base::call` with `this_exact = Sub`, so `$this->m` hits `Sub::m`.
    let src = "<?php\n\
        class Base {\n\
            public function m(int $x): int { return $x; }\n\
            public function call(int $x): int {\n\
                return $this->m($x);\n\
            }\n\
        }\n\
        final class Sub extends Base {\n\
            public function m(int $x): int { return 99; }\n\
        }\n\
        $x = (new Sub())->call(1);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "99");
}

#[test]
fn unknown_receiver_assignment_is_unknown() {
    // Parameter receiver → no exact class → resolve_call_target declines → no summary.
    let src = "<?php\n\
        final class C {\n\
            public function m(int $x): int { return $x; }\n\
        }\n\
        function go($c): void {\n\
            $x = $c->m(1);\n\
            \\PHPStan\\dumpType($x);\n\
        }\n";
    assert_eq!(one_type(src), "unknown");
}

#[test]
fn constructor_assignment_stays_on_exactness_lane() {
    // `$x = new C(1)` is the ADR-0036 object lane, not `__construct`'s value summary.
    let src = "<?php\n\
        final class C {\n\
            public function __construct(int $n) {}\n\
        }\n\
        $x = new C(1);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "C");
}

// Return composition: `return $o->m(...)` / static crosses into an outer summary.

#[test]
fn method_summary_composes_through_function_return() {
    let src = "<?php\n\
        final class C {\n\
            public function g(int $trigger, int $n): int {\n\
                assert($n > 0);\n\
                return $n;\n\
            }\n\
        }\n\
        function f(int $t): int {\n\
            return (new C())->g(1, rand());\n\
        }\n\
        $x = f(9);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "int<1, max>");
}

#[test]
fn static_summary_composes_through_function_return() {
    let src = "<?php\n\
        final class C {\n\
            public static function g(int $trigger, int $n): int {\n\
                assert($n > 0);\n\
                return $n;\n\
            }\n\
        }\n\
        function f(int $t): int {\n\
            return C::g(1, rand());\n\
        }\n\
        $x = f(9);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "int<1, max>");
}

// Declared return floor when the summary degrades to General (function parity).

#[test]
fn method_factless_summary_falls_to_declared_int_floor() {
    // `return rand()` is factless int → arm floor; method must dump `int` too (ADR-0075), not `unknown`.
    let via_method = "<?php\n\
        final class C {\n\
            public function m(int $x): int { return rand(); }\n\
        }\n\
        $x = (new C())->m(1);\n\
        \\PHPStan\\dumpType($x);\n";
    let via_function = "<?php\n\
        function m(int $x): int { return rand(); }\n\
        $x = m(1);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(via_method), "int");
    assert_eq!(one_type(via_method), one_type(via_function));
}

// Opaque may_return: hidden returns join the floor (Composer findPackage shape).

#[test]
fn foreach_hidden_return_does_not_pin_null_on_method() {
    // Composer shape: a loop returns a package, then `return null`. Opaque foreach without
    // may_return→Floor gave Singleton(null) (false call.on-null); the floor join now gives no
    // representable value floor, so no false pin.
    let src = "<?php\n\
        final class Pkg { public function name(): string { return \"p\"; } }\n\
        final class Repo {\n\
            /** @return list<Pkg> */\n\
            public function getPackages(): array { return []; }\n\
            public function findPackage(string $name): ?Pkg {\n\
                foreach ($this->getPackages() as $package) {\n\
                    if ($package->name() === $name) {\n\
                        return $package;\n\
                    }\n\
                }\n\
                return null;\n\
            }\n\
        }\n\
        $repo = new Repo();\n\
        $pkg = $repo->findPackage(\"php\");\n\
        $pkg->name();\n";
    assert_eq!(
        count(src, "call.on-null"),
        0,
        "must not prove $pkg is null when the loop may return a package"
    );
}

#[test]
fn foreach_hidden_return_does_not_pin_null_on_function_twin() {
    // Same hole, free-function form — shared summary machinery, not a method-only special case.
    let src = "<?php\n\
        final class Pkg { public function name(): string { return \"p\"; } }\n\
        /** @param list<Pkg> $packages */\n\
        function findPackage(array $packages, string $name): ?Pkg {\n\
            foreach ($packages as $package) {\n\
                if ($package->name() === $name) {\n\
                    return $package;\n\
                }\n\
            }\n\
            return null;\n\
        }\n\
        $pkg = findPackage([], \"php\");\n\
        $pkg->name();\n";
    assert_eq!(count(src, "call.on-null"), 0);
}

// Asserted stratum does not launder into proof-layer findings.

#[test]
fn asserted_method_summary_does_not_premise_proof_finding() {
    // Body-side Asserted (phpstan-assert helper) crosses as Asserted summary — method twin of
    // `return_summary::mixed_strata_join_renders_asserted`. The proof layer's all-Verified
    // premise rule keeps findings off it.
    let src = "<?php\n\
        /** @phpstan-assert positive-int $v */\n\
        function assertPos($v): void {}\n\
        final class C {\n\
            public function f(int $trigger, $m): int {\n\
                assertPos($m);\n\
                return $m;\n\
            }\n\
        }\n\
        function takesString(string $s): void {}\n\
        $x = (new C())->f(1, rand());\n\
        \\PHPStan\\dumpType($x);\n\
        takesString($x);\n";
    assert_eq!(one_type(src), "int<1, max> (asserted)");
    assert_eq!(
        count(src, ARG_MISMATCH_ID),
        0,
        "Asserted method summary must not premise a proof-layer finding"
    );
}

// Self-assign keeps method declared floor (arms captured before unbind).

#[test]
fn method_self_assign_keeps_declared_int_floor() {
    // `$o = $o->m(1)` unbinds `$o`; arms must be captured at method resolution, while `$o` is still visible.
    let src = "<?php\n\
        final class C {\n\
            public function m(int $x): int { return rand(); }\n\
        }\n\
        $o = new C();\n\
        $o = $o->m(1);\n\
        \\PHPStan\\dumpType($o);\n";
    assert_eq!(one_type(src), "int");
}

// Generators refuse value summaries (ADR-0057 §5).

#[test]
fn generator_method_does_not_rebind_return_value() {
    // `$x = (new C())->g(1)` is a Generator, not 7 — rebinding 7 would premise false mismatches.
    let src = "<?php\n\
        final class C {\n\
            public function g(int $trigger) {\n\
                yield 1;\n\
                return 7;\n\
            }\n\
        }\n\
        function takesObject(object $o): void {}\n\
        $x = (new C())->g(1);\n\
        \\PHPStan\\dumpType($x);\n\
        takesObject($x);\n";
    assert_ne!(one_type(src), "7", "generator call must not rebind the return value");
    assert_eq!(
        count(src, ARG_MISMATCH_ID),
        0,
        "generator result is an object (Generator), not int 7"
    );
}

#[test]
fn generator_function_twin_does_not_rebind_return_value() {
    let src = "<?php\n\
        function g(int $trigger) {\n\
            yield 1;\n\
            return 7;\n\
        }\n\
        function takesObject(object $o): void {}\n\
        $x = g(1);\n\
        \\PHPStan\\dumpType($x);\n\
        takesObject($x);\n";
    assert_ne!(one_type(src), "7");
    assert_eq!(count(src, ARG_MISMATCH_ID), 0);
}

// never / typed fallthrough must not invent Singleton(null).

#[test]
fn never_return_fallthrough_does_not_pin_null() {
    // `: never` leaves scope_return None (unrepresentable, not untyped); fallthrough mustn't contribute Singleton(null).
    let src = "<?php\n\
        function f(int $trigger): never {}\n\
        $x = f(1);\n\
        $x->m();\n";
    assert_eq!(
        count(src, "call.on-null"),
        0,
        ": never fallthrough must not invent null"
    );
}

#[test]
fn object_return_hint_fallthrough_does_not_pin_null() {
    // `: object` also leaves scope_return None but is a written hint — fallthrough is TypeError, not null.
    let src = "<?php\n\
        function f(int $trigger): object {}\n\
        $x = f(1);\n\
        $x->m();\n";
    assert_eq!(count(src, "call.on-null"), 0);
}

// Unrepresentable written return: refuse summary (no A2 oracle).

#[test]
fn object_return_null_does_not_rebind_on_method() {
    // `: object` is a written hint Steins doesn't lower, so the A2 native oracle is empty;
    // without a refuse, `return null` would rebind Singleton(null) and premise call.on-null
    // — a boundary TypeError that never reaches the caller.
    let src = "<?php\n\
        final class C {\n\
            public function m(int $trigger): object {\n\
                return null;\n\
            }\n\
        }\n\
        $x = (new C())->m(1);\n\
        $x->foo();\n";
    assert_eq!(
        count(src, "call.on-null"),
        0,
        ": object {{ return null }} must not rebind Singleton(null)"
    );
}

#[test]
fn object_return_null_does_not_rebind_on_function_twin() {
    let src = "<?php\n\
        function m(int $trigger): object {\n\
            return null;\n\
        }\n\
        $x = m(1);\n\
        $x->foo();\n";
    assert_eq!(count(src, "call.on-null"), 0);
}

// `: mixed` is the one exemption: the total envelope reads as no hint (issue #364).

#[test]
fn mixed_return_hint_rebinds_on_method() {
    // The method leg shares `join_summary` with functions, so the exemption arrives
    // here for free — pinned because "for free" is exactly the kind of claim that
    // stops being true. An exact receiver, a body that proves `3`, a `: mixed` hint
    // that admits it: the call site reads the proof, not the envelope.
    let src = "<?php\n\
        final class C {\n\
            public function m(int $x): mixed { return $x; }\n\
        }\n\
        $x = (new C())->m(3);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(src), "3");
}

#[test]
fn mixed_return_hint_twins_the_hint_less_method() {
    // Same body, three spellings of "promises nothing" — the dumps agree.
    let hint_less = "<?php\n\
        final class C {\n\
            public function m(int $x) { return $x; }\n\
        }\n\
        $x = (new C())->m(3);\n\
        \\PHPStan\\dumpType($x);\n";
    let native_mixed = "<?php\n\
        final class C {\n\
            public function m(int $x): mixed { return $x; }\n\
        }\n\
        $x = (new C())->m(3);\n\
        \\PHPStan\\dumpType($x);\n";
    let phpdoc_mixed = "<?php\n\
        final class C {\n\
            /** @return mixed */\n\
            public function m(int $x) { return $x; }\n\
        }\n\
        $x = (new C())->m(3);\n\
        \\PHPStan\\dumpType($x);\n";
    assert_eq!(one_type(native_mixed), one_type(hint_less));
    assert_eq!(one_type(native_mixed), one_type(phpdoc_mixed));
}

#[test]
fn array_return_null_does_not_rebind_on_method() {
    let src = "<?php\n\
        final class C {\n\
            public function m(int $trigger): array {\n\
                return null;\n\
            }\n\
        }\n\
        $x = (new C())->m(1);\n\
        $x->foo();\n";
    assert_eq!(count(src, "call.on-null"), 0);
}

// Recursion / depth degrade soundly.

#[test]
fn method_recursion_terminates_to_arm_floor() {
    let src = "<?php\n\
        final class C {\n\
            public function f(int $n, bool $b): int {\n\
                if ($b) {\n\
                    return $this->f($n, false);\n\
                }\n\
                return $n;\n\
            }\n\
        }\n\
        $x = (new C())->f(3, true);\n\
        \\PHPStan\\dumpType($x);\n";
    let ty = one_type(src);
    assert!(ty == "int" || ty == "3", "sound + terminating: {ty}");
}
