//! ADR-0077 — the out-parameter seed: a by-reference argument carries a fact
//! only on the branch where the call's result proves the write happened.
//!
//! Three disciplines pinned here:
//!
//! * **Truthiness is the soundness condition, not a precision knob.** Measured
//!   (PHP 8.5.9): `1` assigns the success shape, `0` assigns `[]`, a refused
//!   pattern's `false` writes nothing — so falsy and unguarded paths stay untyped.
//! * **Every premise is proven or the seed refuses**, silently: a literal
//!   pattern the group reader (#149) understands, a flags argument that is
//!   absent or a proven fully-modeled int (issue #168), and a plain local
//!   variable out-parameter.
//! * **Emission stays in the contract layer.** The seeded shape is `Asserted`,
//!   feeding the strict offset leg but never the proof layer.
//!
//! Every shape claim below was measured on PHP 8.5.9.
//!
//! NB: a variable handed to a call is invalidated after that statement, so each
//! fixture dumps a binding once.

use steins_infer::{DEBUG_TYPE_ID, Diagnostic, check};
use steins_syntax::SourceTree;

fn diagnostics(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php")
}

/// Every `debug.type` message body a source produces, in source order.
fn dumps(src: &str) -> Vec<String> {
    diagnostics(src)
        .into_iter()
        .filter(|d| d.id == DEBUG_TYPE_ID)
        .map(|d| d.message.replace("dumped type: ", ""))
        .collect()
}

/// The single dump a one-dump source produces.
fn one_dump(src: &str) -> String {
    let d = dumps(src);
    assert_eq!(d.len(), 1, "expected exactly one dump, got {d:?}");
    d[0].clone()
}

/// `if (preg_match(<pattern>, $s, $m)) { dumpType(<expr>); }` — the truthy branch.
fn guarded(pattern: &str, expr: &str) -> String {
    one_dump(&format!(
        "<?php\nfunction f(string $s): void {{\n\
         if (preg_match({pattern}, $s, $m)) {{ \\PHPStan\\dumpType({expr}); }}\n}}\n"
    ))
}

/// The success shape a pattern seeds, dumped off `$m` itself.
fn shape(pattern: &str) -> String {
    guarded(pattern, "$m")
}

// The shape the callee writes

#[test]
fn a_proven_pattern_seeds_the_success_shape_on_the_truthy_branch() {
    // Measured: whole match plus one key per group, each refined from the
    // sub-pattern that fills it (#156).
    assert_eq!(
        shape(r"'/(\d+)-(\w+)/'"),
        "list{non-falsy-string, numeric-string, non-empty-string} (asserted)"
    );
}

#[test]
fn a_pattern_with_no_groups_writes_the_whole_match_alone() {
    assert_eq!(shape("'/abc/'"), "list{non-falsy-string} (asserted)");
}

#[test]
fn a_trailing_absent_group_is_an_optional_key() {
    // Measured: PHP drops a trailing unmatched group rather than writing `''`.
    // Entries are the enumerated literals (issue #177).
    assert_eq!(shape("'/(a)(b)?/'"), "list{0: non-empty-string, 1: 'a', 2?: 'b'} (asserted)");
}

#[test]
fn an_interior_unmatched_group_stays_a_required_key() {
    // Measured: absence is trailing-only, so the middle group is always
    // present; its `''` joins the enumerated union (issue #177), not `string`.
    assert_eq!(
        shape("'/(a)(b)?(c)/'"),
        "list{non-falsy-string, 'a', ''|'b', 'c'} (asserted)"
    );
}

#[test]
fn a_named_group_occupies_a_string_key_and_a_numeric_one() {
    // Measured: the name is ADDITIONAL, making `array_is_list($m)` false.
    // Group slots spell `non-falsy-numeric-string` since issue #240: numeric
    // AND neither `''` nor `'0'`.
    assert_eq!(
        shape(r"'/(?<year>\d{4})-(?<mon>\d{2})/'"),
        "array{0: non-falsy-string, 1: non-falsy-numeric-string, 2: non-falsy-numeric-string, \
         mon: non-falsy-numeric-string, year: non-falsy-numeric-string} (asserted)"
    );
}

#[test]
fn a_trailing_absent_named_group_leaves_list_ness_open() {
    // Measured both ways on `/(a)(?<b>x)?/`: `'a'` is a list, `'ax'` is not —
    // the fact asserts neither, and the optional string key is why.
    let d = shape("'/(a)(?<b>x)?/'");
    assert_eq!(d, "array{0: non-empty-string, 1: 'a', 2?: 'x', b?: 'x'} (asserted)");
}

#[test]
fn the_seeded_keys_read_back_as_refined_strings() {
    assert_eq!(guarded(r"'/(\d+)/'", "$m[0]"), "numeric-string (asserted)");
    assert_eq!(guarded(r"'/(\d+)/'", "$m[1]"), "numeric-string (asserted)");
}

// The element type, and the absence rule it is coupled to

#[test]
fn a_middle_optional_group_admits_the_empty_string_and_a_trailing_one_does_not() {
    // `(b)*` and `(d)*` are the SAME sub-pattern yet get different element
    // types: an unmatched middle group is present as `''` (measured on `'ac'`:
    // `['ac', 'a', '', 'c']`) while a trailing one is gone — refining the
    // middle group would falsify a reachable path. Groups 1 and 3 enumerate
    // (issue #177); `*`-quantified groups decline since the capture comes
    // from an iteration (measured: `'abbcd'` captures `'b'`, `'d'`).
    assert_eq!(
        shape("'/(a)(b)*(c)(d)*/'"),
        "list{0: non-falsy-string, 1: 'a', 2: string, 3: 'c', \
         4?: non-empty-string} (asserted)"
    );
}

#[test]
fn a_trailing_absent_group_with_a_group_after_it_still_admits_the_empty_string() {
    // `can_be_trailing_absent` is not enough: measured, `'(a)(b)?(c)?/'` on
    // `'ac'` gives `['ac', 'a', '', 'c']` — group 2 is an optional KEY that
    // can still hold `''`, since group 3 may participate where it does not.
    // Only the last group is exempt. (Issue #159's special case for the
    // `list` head was redundant — issue #163's rule already reaches it.)
    assert_eq!(
        shape("'/(a)(b)?(c)?/'"),
        "list{0: non-empty-string, 1: 'a', 2?: ''|'b', 3?: 'c'} (asserted)"
    );
}

#[test]
fn a_two_character_floor_is_what_earns_non_falsy() {
    // The falsy strings are exactly `''` and `'0'`; a floor of two excludes
    // both, a floor of one only `''` — measured, `'([\w-])/'` on `'0'` captures
    // the falsy `'0'`, hence `non-empty-string` and not more.
    assert_eq!(shape("'/ab/'"), "list{non-falsy-string} (asserted)");
    assert_eq!(shape("'/a/'"), "list{non-empty-string} (asserted)");
    assert_eq!(
        shape(r"'/([\w-])/'"),
        "list{non-empty-string, non-empty-string} (asserted)"
    );
    // Measured: `£` is one character but two bytes, so counting characters keeps
    // entry 0 from claiming non-falsy for a one-character capture; the group
    // itself enumerates (issue #177, both currencies captured under `u`).
    assert_eq!(shape("'/(£|€)/u'"), "list{non-empty-string, '£'|'€'} (asserted)");
}

#[test]
fn a_sub_pattern_that_can_only_produce_digits_is_numeric() {
    assert_eq!(
        shape(r"'/x([0-9]+)/'"),
        "list{non-falsy-string, numeric-string} (asserted)"
    );
    // Measured, overturning the obvious reading: `/(\d+)/u` matches `'١٢٣'`
    // (PCRE2 Unicode properties) while `is_numeric('١٢٣')` is `false`.
    assert_eq!(
        shape(r"'/x(\d+)/u'"),
        "list{non-falsy-string, non-empty-string} (asserted)"
    );
    // An explicit ASCII range is unaffected by the modifier.
    assert_eq!(
        shape("'/x([0-9]+)/u'"),
        "list{non-falsy-string, numeric-string} (asserted)"
    );
    // Measured: captures `'...'`.
    assert_eq!(
        shape(r"'/x([\d.]+)/'"),
        "list{non-falsy-string, non-empty-string} (asserted)"
    );
}

#[test]
fn a_sub_pattern_that_can_match_nothing_earns_nothing() {
    assert_eq!(
        shape("'/x(a*)/'"),
        "list{non-empty-string, string} (asserted)"
    );
    // `\K` moves where the overall match starts, so length says nothing about
    // entry 0 — measured, `/a\K0/` on `'a0'` gives the falsy `'0'`.
    assert_eq!(shape(r"'/a\K(b)/'"), "list{string, 'b'} (asserted)");
}

// Where the fact holds, and where it must not

#[test]
fn the_falsy_branch_carries_nothing() {
    // `0` writes `[]`, `false` writes nothing — no single fact covers both.
    let src = "<?php\nfunction f(string $s): void {\n\
               if (preg_match('/(a)/', $s, $m)) { } else { \\PHPStan\\dumpType($m); }\n}\n";
    assert_eq!(one_dump(src), "unknown");
}

#[test]
fn an_unguarded_call_seeds_nothing() {
    // The seed is a refinement, not a transfer: nothing happens AT the call.
    let src = "<?php\nfunction f(string $s): void {\n\
               preg_match('/(a)/', $s, $m);\n\\PHPStan\\dumpType($m);\n}\n";
    assert_eq!(one_dump(src), "unknown");
}

#[test]
fn the_early_return_idiom_carries_the_fact_past_the_guard() {
    // Polarity decides: everything after the guard runs on a proven-truthy call.
    let src = "<?php\nfunction f(string $s): void {\n\
               if (!preg_match('/(a)/', $s, $m)) { return; }\n\\PHPStan\\dumpType($m);\n}\n";
    assert_eq!(one_dump(src), "list{non-empty-string, 'a'} (asserted)");
}

#[test]
fn a_negated_guards_own_branch_carries_nothing() {
    let src = "<?php\nfunction f(string $s): void {\n\
               if (!preg_match('/(a)/', $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n";
    assert_eq!(one_dump(src), "unknown");
}

#[test]
fn an_and_chain_seeds_on_the_branch_where_both_held() {
    let src = "<?php\nfunction f(string $s, bool $b): void {\n\
               if ($b && preg_match('/(a)/', $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n";
    assert_eq!(one_dump(src), "list{non-empty-string, 'a'} (asserted)");
}

#[test]
fn an_or_chain_proves_nothing_on_its_true_branch() {
    // `$b || preg_match(...)` may be true because `$b` was — the call may never
    // have run.
    let src = "<?php\nfunction f(string $s, bool $b): void {\n\
               if ($b || preg_match('/(a)/', $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n";
    assert_eq!(one_dump(src), "unknown");
}

// The refusals (each one silent, each one today's behavior)

/// Every refusal spells the same way: no fact at all.
fn refuses(src: &str) {
    let d = one_dump(src);
    assert_eq!(d, "unknown", "expected a silent refusal, got `{d}`");
}

#[test]
fn an_unproven_pattern_refuses() {
    refuses(
        "<?php\nfunction f(string $s, string $re): void {\n\
         if (preg_match($re, $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n",
    );
}

#[test]
fn a_pattern_the_group_reader_declines_refuses() {
    // The `x` modifier lets a `#` comment swallow a `(`, so the group reader
    // declines — and so does the seed.
    refuses(
        "<?php\nfunction f(string $s): void {\n\
         if (preg_match('/(a)/x', $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n",
    );
}

#[test]
fn an_unproven_flags_argument_refuses() {
    // Issue #168 rule 6: a present flags argument seeds only as a proven int
    // whose every set bit is modeled; a parameter or unmodeled bit (8) declines.
    for flag in ["$flags", "8", "1024"] {
        refuses(&format!(
            "<?php\nfunction f(string $s, int $flags): void {{\n\
             if (preg_match('/(a)/', $s, $m, {flag})) {{ \\PHPStan\\dumpType($m); }}\n}}\n"
        ));
    }
    // Order bits are `preg_match_all` vocabulary: measured (PHP 8.5.9),
    // `PREG_SET_ORDER` throws a `ValueError` here, so it stays outside the mask.
    for flag in ["PREG_PATTERN_ORDER", "PREG_SET_ORDER", "1", "2"] {
        refuses(&format!(
            "<?php\nfunction f(string $s): void {{\n\
             if (preg_match('/(a)/', $s, $m, {flag})) {{ \\PHPStan\\dumpType($m); }}\n}}\n"
        ));
    }
}

#[test]
fn the_unmatched_as_null_flag_turns_optionality_into_nullability() {
    // Issue #168 rule 4, measured: the trailing group is PRESENT as `null`, so
    // the optional key becomes required nullable. Flag resolves by VALUE (512).
    let src = "<?php\nfunction f(string $s): void {\n\
               if (preg_match('/(a)(b)?/', $s, $m, PREG_UNMATCHED_AS_NULL)) { \\PHPStan\\dumpType($m); }\n}\n";
    let with_const = one_dump(src);
    assert_eq!(with_const, "list{non-empty-string, 'a', 'b'|null} (asserted)");
    let src_value = "<?php\nfunction f(string $s): void {\n\
               if (preg_match('/(a)(b)?/', $s, $m, 512)) { \\PHPStan\\dumpType($m); }\n}\n";
    assert_eq!(one_dump(src_value), with_const, "the constant IS its value");
    // Interior group keeps its body refinement, padding gone — measured,
    // `['ac', 'a', null, 'c']`.
    let interior = "<?php\nfunction f(string $s): void {\n\
               if (preg_match('/(a)(b)?(c)/', $s, $m, PREG_UNMATCHED_AS_NULL)) { \\PHPStan\\dumpType($m); }\n}\n";
    // Literal identity under the flag (issue #177): `'b'|null`, not
    // `''|'b'|null` — padding and null never coexist.
    assert_eq!(
        one_dump(interior),
        "list{non-falsy-string, 'a', 'b'|null, 'c'} (asserted)"
    );
}

#[test]
fn the_offset_capture_flag_wraps_every_entry_in_a_measured_pair() {
    // Issue #168 rule 5, probed: a participating group's offset is `>= 0`; an
    // unmatched group's WRITTEN entry is `['', -1]`, reaching the interior
    // can-be-present-empty group. Presence is unchanged: trailing stays dropped.
    let interior = "<?php\nfunction f(string $s): void {\n\
               if (preg_match('/(a)(b)?(c)/', $s, $m, PREG_OFFSET_CAPTURE)) { \\PHPStan\\dumpType($m); }\n}\n";
    // Text slot carries the same `''|'b'` union the flagless entry does.
    assert_eq!(
        one_dump(interior),
        "list{list{non-falsy-string, int<0, max>}, list{'a', int<0, max>}, \
         list{''|'b', int<-1, max>}, list{'c', int<0, max>}} (asserted)"
    );
    let trailing = "<?php\nfunction f(string $s): void {\n\
               if (preg_match('/(a)(b)?/', $s, $m, PREG_OFFSET_CAPTURE)) { \\PHPStan\\dumpType($m); }\n}\n";
    assert_eq!(
        one_dump(trailing),
        "list{0: list{non-empty-string, int<0, max>}, 1: list{'a', int<0, max>}, \
         2?: list{'b', int<0, max>}} (asserted)"
    );
}

#[test]
fn a_userland_twin_of_a_flag_constant_disables_value_resolution() {
    // PHP resolves an unqualified constant through the current namespace first;
    // a project's own `PREG_SET_ORDER` makes the name ambiguous, so no engine
    // value may be assumed.
    refuses(
        "<?php\nnamespace App;\nconst PREG_UNMATCHED_AS_NULL = 0;\n\
         function f(string $s): void {\n\
         if (preg_match('/(a)(b)?/', $s, $m, PREG_UNMATCHED_AS_NULL)) { \\PHPStan\\dumpType($m); }\n}\n",
    );
    // Fully-qualified spelling names the engine constant regardless: it's
    // defined first, and redefining an existing constant is a no-op.
    let fq = "<?php\nnamespace App;\nconst PREG_UNMATCHED_AS_NULL = 0;\n\
              function f(string $s): void {\n\
              if (preg_match('/(a)(b)?/', $s, $m, \\PREG_UNMATCHED_AS_NULL)) { \\PHPStan\\dumpType($m); }\n}\n";
    assert_eq!(one_dump(fq), "list{non-empty-string, 'a', 'b'|null} (asserted)");
}

#[test]
fn a_proven_zero_flags_argument_is_modelled() {
    // Measured: explicit `0` writes what an absent flags argument does;
    // `$offset` (position 4) moves the match start without touching the keys.
    let src = "<?php\nfunction f(string $s): void {\n\
               if (preg_match('/(a)/', $s, $m, 0, 1)) { \\PHPStan\\dumpType($m); }\n}\n";
    assert_eq!(one_dump(src), "list{non-empty-string, 'a'} (asserted)");
}

#[test]
fn preg_match_all_seeds_through_the_same_witness() {
    // Slice D (issue #168): `preg_match_all` shares `ReturnTruthy` (shape
    // pinned in `preg_match_all_seed.rs`); here only the seam.
    let src = "<?php\nfunction f(string $s): void {\n\
               if (preg_match_all('/(a)/', $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n";
    assert_eq!(
        one_dump(src),
        "list{non-empty-list<non-empty-string>, non-empty-list<'a'>} (asserted)"
    );
    refuses(
        "<?php\nfunction f(string $s): void {\n\
         preg_match_all('/(a)/', $s, $m);\n\\PHPStan\\dumpType($m);\n}\n",
    );
}

#[test]
fn a_missing_out_parameter_refuses() {
    // The arity leg: an argument the call never supplied was never written.
    let src = "<?php\nfunction f(string $s): void {\n\
               $m = 1;\nif (preg_match('/(a)/', $s)) { \\PHPStan\\dumpType($m); }\n}\n";
    assert_eq!(one_dump(src), "1");
}

#[test]
fn a_property_out_parameter_refuses() {
    // ADR-0077 §3.6: the write may be visible to callers this scope cannot see.
    let src = "<?php\nclass C {\npublic array $m = [];\n\
               public function f(string $s): void {\n\
               if (preg_match('/(a)/', $s, $this->m)) { \\PHPStan\\dumpType($this->m); }\n}\n}\n";
    let d = one_dump(src);
    assert!(!d.contains("non-empty-"), "a property out-parameter must not be seeded: {d}");
}

#[test]
fn an_array_offset_out_parameter_refuses() {
    let src = "<?php\nfunction f(string $s, array $a): void {\n\
               if (preg_match('/(a)/', $s, $a['k'])) { \\PHPStan\\dumpType($a['k']); }\n}\n";
    let d = one_dump(src);
    assert!(!d.contains("non-empty-"), "an offset out-parameter must not be seeded: {d}");
}

#[test]
fn a_named_argument_refuses() {
    // Positional mapping is defeated, so no position can say which argument it is.
    refuses(
        "<?php\nfunction f(string $s): void {\n\
         if (preg_match('/(a)/', $s, matches: $m)) { \\PHPStan\\dumpType($m); }\n}\n",
    );
}

#[test]
fn a_userland_shadow_is_a_different_function() {
    // Same recognition discipline as the type/array-predicate guards: a
    // project-defined twin has its own contract, unknown here.
    let src = "<?php\nnamespace App;\nfunction preg_match($p, $s, &$m): int { return 1; }\n\
               function f(string $s): void {\n\
               if (preg_match('/(a)/', $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n";
    let d = dumps(src);
    assert!(
        d.iter().all(|t| !t.contains("list{")),
        "a userland twin must not seed the builtin's shape: {d:?}"
    );
}

#[test]
fn a_fully_qualified_spelling_seeds_the_same_shape() {
    // `\preg_match(...)` inside a namespace is the global builtin, and seeds
    // exactly what the unqualified spelling does (issue #153).
    let src = "<?php\nnamespace App;\nfunction f(string $s): void {\n\
               if (\\preg_match('/(a)/', $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n";
    assert_eq!(one_dump(src), "list{non-empty-string, 'a'} (asserted)");
}

#[test]
fn a_fully_qualified_spelling_reaches_past_a_same_namespace_homonym() {
    // Measured (PHP 8.5.9): with `App\is_string` declared alongside,
    // `\is_string("x")` still returns the global builtin's `true` — leading
    // `\` beats the shadow, so the seed stands here too.
    let src = "<?php\nnamespace App;\nfunction preg_match($p, $s, &$m): int { return 1; }\n\
               function f(string $s): void {\n\
               if (\\preg_match('/(a)/', $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n";
    assert_eq!(one_dump(src), "list{non-empty-string, 'a'} (asserted)");
}

#[test]
fn a_namespaced_twin_is_a_different_function() {
    // `App\preg_match` is a name of its own — nothing here knows its contract.
    let src = "<?php\nnamespace App;\nfunction f(string $s): void {\n\
               if (\\App\\preg_match('/(a)/', $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n";
    let d = dumps(src);
    assert!(
        d.iter().all(|t| !t.contains("list{")),
        "a namespaced twin must not seed the builtin's shape: {d:?}"
    );
}

#[test]
fn a_namespace_relative_spelling_is_a_different_function() {
    // `namespace\preg_match` resolves to `App\preg_match` ONLY, no global
    // fallback (measured, PHP 8.5.9: fatal). The stripped prefix means only
    // reference *kind* tells this spelling apart from the global one.
    let src = "<?php\nnamespace App;\nfunction f(string $s): void {\n\
               if (namespace\\preg_match('/(a)/', $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n";
    let d = dumps(src);
    assert!(
        d.iter().all(|t| !t.contains("list{")),
        "a namespace-relative twin must not seed the builtin's shape: {d:?}"
    );
}

#[test]
fn an_aliased_import_is_a_different_function() {
    // `use function Other\thing as preg_match;` sends the unqualified call to
    // `Other\thing`, no fallback (measured: fatal, naming `Other\thing()`).
    let src = "<?php\nnamespace App;\nuse function Other\\thing as preg_match;\n\
               function f(string $s): void {\n\
               if (preg_match('/(a)/', $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n";
    let d = dumps(src);
    assert!(
        d.iter().all(|t| !t.contains("list{")),
        "an aliased import must not seed the builtin's shape: {d:?}"
    );
}

// Emission discipline

#[test]
fn a_seed_feeds_the_strict_leg_and_never_the_proof_layer() {
    // The sealed shape is a real claim: `$m[3]` cannot exist for a two-group
    // pattern, and the contract layer says so; the proof layer, judging only
    // proven whole values, must stay silent — the seed is `Asserted`.
    let src = "<?php\nfunction f(string $s): void {\n\
               if (preg_match('/(a)(b)/', $s, $m)) { $x = $m[3]; }\n}\n";
    let ds = diagnostics(src);
    assert!(
        ds.iter().any(|d| d.id == "offset.undeclared"),
        "the sealed seed must reach the contract-layer offset leg: {ds:?}"
    );
    assert!(
        ds.iter().all(|d| d.id != "offset.missing" && d.id != "offset.on-unsupported"),
        "an Asserted seed must never premise the proof layer: {ds:?}"
    );
}

#[test]
fn seeded_reads_emit_nothing_but_the_strict_leg() {
    let strict_leg = ["offset.undeclared", "offset.maybe-missing"];
    let bodies = [
        "$x = $m[0];",
        "$x = $m[1];",
        "$x = $m[2];",
        "$x = $m['nope'];",
        "$n = count($m);",
        "foreach ($m as $k => $e) { $y = $e; }",
        "$x = $m[1] ?? 'd';",
        "if ($m) { return; }",
    ];
    for body in bodies {
        for pattern in ["'/(a)/'", "'/(a)(b)?/'", r"'/(?<n>a)/'", "'/abc/'"] {
            let src = format!(
                "<?php\nfunction f(string $s): void {{\n\
                 if (preg_match({pattern}, $s, $m)) {{ {body} }}\n}}\n"
            );
            let ds = diagnostics(&src);
            let found: Vec<&Diagnostic> = ds
                .iter()
                .filter(|d| !d.id.starts_with("debug.") && !strict_leg.contains(&d.id))
                .collect();
            assert!(found.is_empty(), "`{pattern}` + `{body}` emitted {found:?}");
        }
    }
}
