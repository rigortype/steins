//! ADR-0077 — the out-parameter seed: a by-reference argument carries a fact
//! only on the branch where the call's result proves the write happened.
//!
//! Three disciplines are pinned here, not just the spellings:
//!
//! * **Truthiness is the soundness condition, not a precision knob.** Measured
//!   (PHP 8.5.9): `preg_match` returns `1` and assigns the success shape, `0` and
//!   assigns `[]`, and — on a pattern PCRE refuses to compile — `false`, assigning
//!   **nothing at all**, so the caller's variable keeps whatever it held. There is
//!   no unconditional fact to state, which is why the falsy branch and every
//!   unguarded path below must stay exactly as untyped as they were.
//! * **Every premise is proven or the seed refuses**, silently: the pattern is a
//!   literal the group reader (#149) fully understands, the flags argument is
//!   absent or a proven `0`, and the out-parameter is a plain local variable.
//! * **Emission stays in the contract layer.** The seeded shape is `Asserted`, so
//!   it may feed the strict offset leg (a read of a key the sealed shape excludes)
//!   and must never reach the proof layer.
//!
//! Every shape claim below was produced by running PHP 8.5.9 — the key sets, the
//! trailing-versus-interior absence split, the double key of a named group, and
//! the `array_is_list` verdicts.
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

// ---- The shape the callee writes -------------------------------------------

#[test]
fn a_proven_pattern_seeds_the_success_shape_on_the_truthy_branch() {
    // `preg_match('/(\d+)-(\w+)/', '12-ab', $m)` measured `[0 => '12-ab',
    // 1 => '12', 2 => 'ab']`: the whole match plus one key per group, all strings.
    assert_eq!(
        shape(r"'/(\d+)-(\w+)/'"),
        "non-empty-list{string, string, string} (asserted)"
    );
}

#[test]
fn a_pattern_with_no_groups_writes_the_whole_match_alone() {
    assert_eq!(shape("'/abc/'"), "non-empty-list{string} (asserted)");
}

#[test]
fn a_trailing_absent_group_is_an_optional_key() {
    // `preg_match('/(a)(b)?/', 'a', $m)` measured keys `[0, 1]` — PHP DROPS a
    // trailing unmatched group rather than writing `''` for it.
    assert_eq!(
        shape("'/(a)(b)?/'"),
        "non-empty-list{string, string, 2?: string} (asserted)"
    );
}

#[test]
fn an_interior_unmatched_group_stays_a_required_key() {
    // `preg_match('/(a)(b)?(c)/', 'ac', $m)` measured `[0, 1, 2 => '', 3]`:
    // absence is a trailing-only phenomenon, so the middle group is always there.
    assert_eq!(
        shape("'/(a)(b)?(c)/'"),
        "non-empty-list{string, string, string, string} (asserted)"
    );
}

#[test]
fn a_named_group_occupies_a_string_key_and_a_numeric_one() {
    // `preg_match('/(?<year>\d{4})-(?<mon>\d{2})/', '2026-08', $m)` measured
    // `[0, 'year', 1, 'mon', 2]` — the name is ADDITIONAL, never a replacement,
    // and it makes `array_is_list($m)` false (measured), so the fact is no list.
    assert_eq!(
        shape(r"'/(?<year>\d{4})-(?<mon>\d{2})/'"),
        "non-empty-array{string, string, string, mon: string, year: string} (asserted)"
    );
}

#[test]
fn a_trailing_absent_named_group_leaves_list_ness_open() {
    // Measured both ways on `/(a)(?<b>x)?/`: `'a'` gives `[0, 1]` (a list), `'ax'`
    // gives `[0, 1, 'b', 2]` (not one). Neither verdict is available, so the fact
    // asserts neither — and the optional string key is what says so.
    let d = shape("'/(a)(?<b>x)?/'");
    assert_eq!(d, "non-empty-array{string, string, 2?: string, b?: string} (asserted)");
}

#[test]
fn the_seeded_keys_read_back_as_strings() {
    assert_eq!(guarded(r"'/(\d+)/'", "$m[0]"), "string (asserted)");
    assert_eq!(guarded(r"'/(\d+)/'", "$m[1]"), "string (asserted)");
}

// ---- Where the fact holds, and where it must not ---------------------------

#[test]
fn the_falsy_branch_carries_nothing() {
    // `0` writes `[]` and `false` writes nothing at all; no single fact covers
    // both, and the engine must not invent one.
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
    // `if (!preg_match(...)) { return; }` — the polarity decides, not the branch:
    // everything after the guard runs on a proven-truthy call.
    let src = "<?php\nfunction f(string $s): void {\n\
               if (!preg_match('/(a)/', $s, $m)) { return; }\n\\PHPStan\\dumpType($m);\n}\n";
    assert_eq!(one_dump(src), "non-empty-list{string, string} (asserted)");
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
    assert_eq!(one_dump(src), "non-empty-list{string, string} (asserted)");
}

#[test]
fn an_or_chain_proves_nothing_on_its_true_branch() {
    // `$b || preg_match(...)` may be true because `$b` was: the call may never
    // have run, let alone written.
    let src = "<?php\nfunction f(string $s, bool $b): void {\n\
               if ($b || preg_match('/(a)/', $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n";
    assert_eq!(one_dump(src), "unknown");
}

// ---- The refusals (each one silent, each one today's behavior) -------------

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
    // The `x` modifier changes what counts as a group (a `#` comment can swallow
    // a `(`), so the reader declines — and so does the seed.
    refuses(
        "<?php\nfunction f(string $s): void {\n\
         if (preg_match('/(a)/x', $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n",
    );
}

#[test]
fn a_flags_argument_refuses() {
    // `PREG_OFFSET_CAPTURE` makes every entry a `[string, int]` pair and
    // `PREG_UNMATCHED_AS_NULL` turns the trailing absence into an explicit null
    // (both measured). Neither is modelled, so both refuse (ADR-0077 §4).
    for flag in ["PREG_OFFSET_CAPTURE", "PREG_UNMATCHED_AS_NULL", "$flags"] {
        refuses(&format!(
            "<?php\nfunction f(string $s, int $flags): void {{\n\
             if (preg_match('/(a)/', $s, $m, {flag})) {{ \\PHPStan\\dumpType($m); }}\n}}\n"
        ));
    }
}

#[test]
fn a_proven_zero_flags_argument_is_modelled() {
    // Measured: an explicit `0` writes exactly what an absent flags argument does,
    // and `$offset` (position 4) moves where matching starts without touching the
    // written keys.
    let src = "<?php\nfunction f(string $s): void {\n\
               if (preg_match('/(a)/', $s, $m, 0, 1)) { \\PHPStan\\dumpType($m); }\n}\n";
    assert_eq!(one_dump(src), "non-empty-list{string, string} (asserted)");
}

#[test]
fn preg_match_all_refuses() {
    // Its witness is the same truthiness, but `PREG_PATTERN_ORDER` versus
    // `PREG_SET_ORDER` doubles the written shape and neither is measured here.
    refuses(
        "<?php\nfunction f(string $s): void {\n\
         if (preg_match_all('/(a)/', $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n",
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
    // The same recognition discipline the type-predicate and array-predicate
    // guards carry: a project-defined twin has its own contract, and nothing here
    // knows it.
    let src = "<?php\nnamespace App;\nfunction preg_match($p, $s, &$m): int { return 1; }\n\
               function f(string $s): void {\n\
               if (preg_match('/(a)/', $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n";
    let d = dumps(src);
    assert!(
        d.iter().all(|t| !t.contains("non-empty-list")),
        "a userland twin must not seed the builtin's shape: {d:?}"
    );
}

#[test]
fn a_fully_qualified_spelling_seeds_the_same_shape() {
    // `\preg_match(...)` inside a namespace is the global builtin — the spelling a
    // namespaced file uses when it wants the global function unambiguously — and
    // it seeds exactly what the unqualified spelling does (issue #153).
    let src = "<?php\nnamespace App;\nfunction f(string $s): void {\n\
               if (\\preg_match('/(a)/', $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n";
    assert_eq!(one_dump(src), "non-empty-list{string, string} (asserted)");
}

#[test]
fn a_fully_qualified_spelling_reaches_past_a_same_namespace_homonym() {
    // Measured (php 8.5.9): with an `App\is_string` declared alongside,
    // `\is_string("x")` still answers the global builtin's `true`. The leading `\`
    // is what makes the shadow irrelevant, so the seed stands here too.
    let src = "<?php\nnamespace App;\nfunction preg_match($p, $s, &$m): int { return 1; }\n\
               function f(string $s): void {\n\
               if (\\preg_match('/(a)/', $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n";
    assert_eq!(one_dump(src), "non-empty-list{string, string} (asserted)");
}

#[test]
fn a_namespaced_twin_is_a_different_function() {
    // `App\preg_match` is a name of its own, whether or not the project declares
    // it — nothing here knows its contract.
    let src = "<?php\nnamespace App;\nfunction f(string $s): void {\n\
               if (\\App\\preg_match('/(a)/', $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n";
    let d = dumps(src);
    assert!(
        d.iter().all(|t| !t.contains("non-empty-list")),
        "a namespaced twin must not seed the builtin's shape: {d:?}"
    );
}

#[test]
fn a_namespace_relative_spelling_is_a_different_function() {
    // `namespace\preg_match` resolves to `App\preg_match` ONLY, with no global
    // fallback — measured on php 8.5.9 as a fatal "Call to undefined function".
    // The `namespace\` prefix is stripped from the stored raw name, so only the
    // reference *kind* can tell this spelling apart from the global one.
    let src = "<?php\nnamespace App;\nfunction f(string $s): void {\n\
               if (namespace\\preg_match('/(a)/', $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n";
    let d = dumps(src);
    assert!(
        d.iter().all(|t| !t.contains("non-empty-list")),
        "a namespace-relative twin must not seed the builtin's shape: {d:?}"
    );
}

#[test]
fn an_aliased_import_is_a_different_function() {
    // `use function Other\thing as preg_match;` sends the unqualified call to
    // `Other\thing` with no fallback (measured: a fatal naming `Other\thing()`).
    let src = "<?php\nnamespace App;\nuse function Other\\thing as preg_match;\n\
               function f(string $s): void {\n\
               if (preg_match('/(a)/', $s, $m)) { \\PHPStan\\dumpType($m); }\n}\n";
    let d = dumps(src);
    assert!(
        d.iter().all(|t| !t.contains("non-empty-list")),
        "an aliased import must not seed the builtin's shape: {d:?}"
    );
}

// ---- Emission discipline ---------------------------------------------------

#[test]
fn a_seed_feeds_the_strict_leg_and_never_the_proof_layer() {
    // The sealed shape is a real claim: `$m[3]` cannot exist for a two-group
    // pattern, and the contract layer says so. The proof layer, which judges only
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
