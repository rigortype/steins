//! Issue #595 — `settype` at STATEMENT position writes the cast fact instead of
//! being forgotten.
//!
//! Three disciplines pinned here:
//!
//! * **The witness is the call RETURNING, not a truthiness test.** Measured
//!   (PHP 8.5.9): `settype` answers `true` for every pair that returns at all
//!   and `false` for none, so the two non-writing outcomes are both non-returns
//!   (a `ValueError` on an unrecognized type string, an `Error` on an object
//!   under `'string'`). Control reaching the next statement is the proof.
//! * **Every premise is proven or the seed refuses**, silently: the engine's own
//!   declaration and arity, a proven literal type string, a target the value
//!   domain can spell, a plain local variable, an unpoisoned scope, and a
//!   pre-call claim about the input. A refusal leaves the by-ref invalidation
//!   standing — the FP-safe floor.
//! * **The stratum is the input's**, not the seed's: an `Asserted` phpdoc input
//!   stays `Asserted` through the cast, and a `Verified` one stays `Verified`.
//!
//! The cast grid itself is unit-tested in `coerce.rs`; the fixtures here are
//! about the seam, not about re-listing the table.

use std::collections::HashMap;

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, Folder, check_with};
use steins_syntax::SourceTree;

/// A mock PHP answering exactly the reflected declaration the `settype` row is
/// pinned against, measured at PHP 8.5.9 with `ReflectionFunction`:
/// `settype(mixed &$var, string $type): bool`, two parameters, both required.
#[derive(Default)]
struct Mock {
    types: HashMap<String, String>,
    arities: HashMap<String, (u32, u32)>,
}

impl Mock {
    fn sidecar() -> Mock {
        Mock {
            types: HashMap::from([("settype".to_owned(), "bool".to_owned())]),
            arities: HashMap::from([("settype".to_owned(), (2, 2))]),
        }
    }

    /// An engine that has moved: same name, a different signature. Nothing this
    /// rule was written against still holds, so the row must withhold.
    fn moved() -> Mock {
        Mock {
            types: HashMap::from([("settype".to_owned(), "bool".to_owned())]),
            arities: HashMap::from([("settype".to_owned(), (3, 2))]),
        }
    }
}

impl Folder for Mock {
    fn fold(
        &mut self,
        _name: &str,
        _args: &[steins_syntax::ArgValue],
        _strict: bool,
    ) -> Option<steins_syntax::ArgValue> {
        None
    }
    fn builtin_return_type(&mut self, name: &str) -> Option<String> {
        self.types.get(&name.to_ascii_lowercase()).cloned()
    }
    fn builtin_param_counts(&mut self, name: &str) -> Option<(u32, u32)> {
        self.arities.get(&name.to_ascii_lowercase()).copied()
    }
}

fn diagnostics_with(src: &str, folder: &mut Mock) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    // Drop `untyped.*` (ADR-0078, #200): it flags the fixtures' own deliberately
    // untyped signatures, not the behavior under test.
    check_with(&tree, &[], "t.php", folder)
        .into_iter()
        .filter(|d| !d.id.starts_with("untyped."))
        .collect()
}

/// Every `debug.type` body a source produces, in source order.
fn dumps_with(src: &str, folder: &mut Mock) -> Vec<String> {
    diagnostics_with(src, folder)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect()
}

fn dumps(src: &str) -> Vec<String> {
    dumps_with(src, &mut Mock::sidecar())
}

/// The single dump a one-dump source produces, asserting no other finding came
/// with it — a seeded fact must never premise one.
fn one_dump(src: &str) -> String {
    let ds = diagnostics_with(src, &mut Mock::sidecar());
    let other: Vec<&Diagnostic> = ds.iter().filter(|d| !d.id.starts_with("debug.")).collect();
    assert!(other.is_empty(), "a settype seed emitted a finding: {other:?}");
    let d: Vec<String> = ds
        .iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect();
    assert_eq!(d.len(), 1, "expected exactly one dump, got {d:?}");
    d[0].clone()
}

/// `function f(<param>): void { settype($v, <type>); dumpType($v); }` — the
/// declared-lane fixture, where `$v` reaches the call with only its declaration.
fn declared(param: &str, ty: &str) -> String {
    one_dump(&format!(
        "<?php\nfunction f({param}): void {{ settype($v, {ty}); \\PHPStan\\dumpType($v); }}\n"
    ))
}

/// `$v = <literal>; settype($v, <type>); dumpType($v);` — the value-lane fixture.
fn literal(value: &str, ty: &str) -> String {
    one_dump(&format!(
        "<?php\nfunction f(): void {{ $v = {value}; settype($v, {ty}); \\PHPStan\\dumpType($v); }}\n"
    ))
}

// The witness

#[test]
fn the_issue_witness_renders_int() {
    // The headline: `settype($s, 'int')` at statement position leaves `int`
    // where the by-ref invalidation used to leave `unknown`.
    assert_eq!(declared("string $v", "'int'"), "int");
}

#[test]
fn every_spellable_target_writes_its_column() {
    assert_eq!(declared("string $v", "'integer'"), "int");
    assert_eq!(declared("string $v", "'float'"), "float");
    assert_eq!(declared("string $v", "'double'"), "float");
    assert_eq!(declared("int $v", "'string'"), "numeric-uncased-string");
    assert_eq!(declared("string $v", "'bool'"), "bool");
    assert_eq!(declared("string $v", "'boolean'"), "bool");
    assert_eq!(declared("string $v", "'array'"), "list{string}");
    assert_eq!(declared("string $v", "'null'"), "null");
}

#[test]
fn a_float_declaration_is_read_as_what_the_slot_holds() {
    // `steins_contract::to_fact` floors a `float` arm for slot ADMISSION (a
    // float declaration accepts an int); what the slot HOLDS is a float on
    // every path PHP can reach it by, which is what the cast consumes.
    assert_eq!(declared("float $v", "'int'"), "int");
    assert_eq!(declared("float $v", "'string'"), "non-empty-uppercase-string");
    assert_eq!(declared("float $v", "'bool'"), "bool");
}

#[test]
fn a_bool_input_enumerates_the_two_values_it_casts_to() {
    assert_eq!(declared("bool $v", "'int'"), "0|1");
    assert_eq!(declared("bool $v", "'float'"), "0.0|1.0");
    assert_eq!(declared("bool $v", "'string'"), "''|'1'");
}

#[test]
fn a_proven_value_casts_value_precisely() {
    assert_eq!(literal("123", "'string'"), "'123'");
    assert_eq!(literal("123", "'float'"), "123.0");
    assert_eq!(literal("123.0", "'int'"), "123");
    assert_eq!(literal("'5'", "'int'"), "5");
    assert_eq!(literal("true", "'int'"), "1");
    assert_eq!(literal("false", "'string'"), "''");
    assert_eq!(literal("null", "'int'"), "0");
    assert_eq!(literal("null", "'string'"), "''");
    // `null` is the one input that becomes the EMPTY array, not a wrapped one.
    assert_eq!(literal("null", "'array'"), "array{}");
    assert_eq!(literal("'x'", "'array'"), "list{'x'}");
}

#[test]
fn the_bool_target_reads_php_truthiness() {
    assert_eq!(literal("'some-string'", "'bool'"), "true");
    assert_eq!(literal("'0'", "'bool'"), "false");
    assert_eq!(literal("0", "'bool'"), "false");
    assert_eq!(literal("123", "'bool'"), "true");
}

#[test]
fn a_cast_to_the_same_type_is_the_identity() {
    assert_eq!(literal("'keep'", "'string'"), "'keep'");
    assert_eq!(literal("42", "'int'"), "42");
}

// The refusals — every one leaves the by-ref invalidation standing

#[test]
fn an_unproven_type_string_refuses() {
    assert_eq!(
        one_dump(
            "<?php\nfunction f(string $v, string $t): void \
             { settype($v, $t); \\PHPStan\\dumpType($v); }\n"
        ),
        "unknown"
    );
}

#[test]
fn a_type_string_php_itself_refuses_writes_nothing() {
    // Measured: `'real'`, `'binary'`, `' int'` and `''` each raise
    // `ValueError: settype(): Argument #2 ($type) must be a valid type`.
    for ty in ["'real'", "'binary'", "' int'", "'foo'", "''"] {
        assert_eq!(declared("string $v", ty), "unknown", "{ty} is not a type");
    }
}

#[test]
fn the_object_target_refuses_for_want_of_a_fact() {
    // php-src accepts `'object'` and writes a `stdClass`; the value domain has
    // no object layer to state that in, so the name stays forgotten.
    assert_eq!(declared("string $v", "'object'"), "unknown");
}

#[test]
fn an_array_to_string_cast_refuses() {
    // Measured: an `E_WARNING` and the literal `'Array'` — not a fact worth
    // speaking for a program that is already wrong.
    assert_eq!(literal("['foo']", "'string'"), "unknown");
}

#[test]
fn a_property_argument_refuses() {
    // The ADR-0077 §3.6 aliasing leg, verbatim: the write may be visible to
    // callers this scope cannot see.
    let d = dumps(
        "<?php\nclass C { public string $p = 'x'; }\n\
         function f(C $c): void { settype($c->p, 'int'); \\PHPStan\\dumpType($c->p); }\n",
    );
    assert_eq!(d, vec!["unknown".to_owned()]);
}

#[test]
fn an_array_offset_argument_refuses() {
    // The same leg for `$a['k']`. The claim is that the CAST wrote nothing: the
    // array still reads as the literal built it, with no `int` anywhere in it.
    // That the enclosing binding survives the by-ref call at all is the
    // statement-invalidation question (ADR-0063 §2.3) and predates this row.
    let d = dumps(
        "<?php\nfunction f(): void \
         { $a = ['x']; settype($a[0], 'int'); \\PHPStan\\dumpType($a); }\n",
    );
    assert_eq!(d, vec!["list{'x'}".to_owned()]);
}

#[test]
fn a_poisoned_scope_refuses() {
    // ADR-0046: `extract()` may rewrite the frame the seed would land in, so the
    // whole scope loses the right to say which binding a name is.
    let d = dumps(
        "<?php\nfunction f(string $v, array $a): void \
         { extract($a); settype($v, 'int'); \\PHPStan\\dumpType($v); }\n",
    );
    assert_eq!(d, vec!["unknown".to_owned()]);
}

#[test]
fn an_assignment_position_call_refuses() {
    // `$v = settype($v, 'int')` writes the cast AND THEN overwrites `$v` with
    // the call's `true`; the last word is the assignment's, so the statement
    // rung stays out of it entirely.
    let d = dumps(
        "<?php\nfunction f(string $v): void { $v = settype($v, 'int'); \\PHPStan\\dumpType($v); }\n",
    );
    assert_eq!(d, vec!["unknown".to_owned()]);
}

#[test]
fn a_named_or_spread_argument_refuses() {
    // `out_param_seed_callee`'s positional gate: nothing here could say which
    // argument is which.
    assert_eq!(
        one_dump(
            "<?php\nfunction f(string $v): void \
             { settype(var: $v, type: 'int'); \\PHPStan\\dumpType($v); }\n"
        ),
        "unknown"
    );
}

#[test]
fn a_namespaced_or_shadowed_settype_is_a_different_function() {
    // `global_function_callee`: a project function of the same simple name is
    // not the builtin, and asking the catalog about it would claim something
    // about code this walk did not analyze.
    assert_eq!(
        one_dump(
            "<?php\nnamespace N;\nfunction settype(&$v, string $t): bool { return true; }\n\
             function f(string $v): void { settype($v, 'int'); \\PHPStan\\dumpType($v); }\n"
        ),
        "unknown"
    );
}

#[test]
fn a_silent_engine_withholds_the_whole_row() {
    // ADR-0061 §2: no reflected declaration, no admission — the sound subset
    // (ADR-0004) rather than a rule trusted on nothing.
    let src = "<?php\nfunction f(string $v): void { settype($v, 'int'); \\PHPStan\\dumpType($v); }\n";
    assert_eq!(dumps_with(src, &mut Mock::default()), vec!["unknown".to_owned()]);
}

#[test]
fn a_moved_signature_withholds_the_whole_row() {
    // ADR-0064 Amendment B: the parameter this rule writes is declared `mixed`,
    // which pins nothing on its own, so the arity is the countersignature — and
    // it is a claim about this engine that can fail.
    let src = "<?php\nfunction f(string $v): void { settype($v, 'int'); \\PHPStan\\dumpType($v); }\n";
    assert_eq!(dumps_with(src, &mut Mock::moved()), vec!["unknown".to_owned()]);
}

// Stratum

#[test]
fn an_asserted_input_stays_asserted_through_the_cast() {
    // The phpdoc claim is the cast's only premise about the value, so the fact
    // it produces may silence but never premise a proof.
    assert_eq!(
        one_dump(
            "<?php\n/** @param numeric-string $v */\nfunction f(string $v): void \
             { settype($v, 'int'); \\PHPStan\\dumpType($v); }\n"
        ),
        "int (asserted)"
    );
}

#[test]
fn a_verified_input_stays_verified() {
    // No `(asserted)` marker: a native declaration and a measured conversion.
    assert_eq!(declared("string $v", "'int'"), "int");
    assert_eq!(literal("123", "'string'"), "'123'");
}

// The seam

#[test]
fn the_written_fact_survives_into_the_next_statement() {
    let d = dumps(
        "<?php\nfunction f(string $v): void \
         { settype($v, 'int'); $x = 1; \\PHPStan\\dumpType($v); }\n",
    );
    assert_eq!(d, vec!["int".to_owned()]);
}

#[test]
fn a_second_cast_reads_the_first_ones_output() {
    // The chain is the whole point of writing into the env rather than reporting
    // once: `string` to `int` to `'1'`-or-`''`.
    assert_eq!(
        one_dump(
            "<?php\nfunction f(string $v): void \
             { settype($v, 'int'); settype($v, 'bool'); settype($v, 'string'); \
             \\PHPStan\\dumpType($v); }\n"
        ),
        "''|'1'"
    );
}

#[test]
fn a_cast_inside_a_branch_joins_with_the_untouched_path() {
    // The seed lands on the guarded branch's env like any other binding, so the
    // fall-through join sees `int` from one path and the untouched declaration
    // from the other — the abstract union layer (issue #339) holds exactly that.
    assert_eq!(
        one_dump(
            "<?php\nfunction f(string $v, bool $c): void \
             { if ($c) { settype($v, 'int'); } \\PHPStan\\dumpType($v); }\n"
        ),
        "int|string"
    );
}
