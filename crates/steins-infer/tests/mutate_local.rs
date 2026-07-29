//! ADR-0063 P2 acceptance tests: the `mutate.local` color, the **conditional**
//! by-ref out-parameter rows, and the Pure tolerance.
//!
//! The thesis under test is that an out-parameter write is a property of the
//! *call*, not of the function. Every test here therefore comes in pairs that
//! differ only in an argument: supplied vs. omitted (the arity leg), and a frame
//! local vs. a property or superglobal (the target leg). A per-function flag
//! could not tell either pair apart — which is the upstream lesson ADR-0063
//! imports (php-src #11884: conditional on the argument, not a per-function lie).
//!
//! The tolerance leg is the other half: `preg_match` into a local is what a pure
//! function does all day, and 狼少年撲滅 means it must not bark.

use steins_infer::{check, effect_summary, Diagnostic, EffectSummary, EFFECT_ID, PARAM_MISMATCH_ID};
use steins_syntax::SourceTree;

fn effects(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == EFFECT_ID).collect()
}

fn one(src: &str) -> Diagnostic {
    let f = effects(src);
    assert_eq!(f.len(), 1, "expected exactly one effect finding, got: {f:#?}");
    f.into_iter().next().unwrap()
}

fn summary(src: &str, symbol: &str) -> EffectSummary {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == symbol)
        .unwrap_or_else(|| panic!("no summary for {symbol}"))
}

// ---- The label exists and is registered -------------------------------------

#[test]
fn mutate_local_is_a_registered_label_under_mutate() {
    assert!(steins_catalog::is_known_label("mutate.local"));
    // It is a child of `mutate`, so a coarse declaration admits it (ADR-0018).
    assert!(steins_catalog::subsumes("mutate", "mutate.local"));
    assert!(!steins_catalog::subsumes("mutate.local", "mutate"));
}

#[test]
fn declaring_mutate_local_is_accepted_by_the_registry() {
    // The whole point of registering it: `#[\Steins\Effect('mutate.local')]` must
    // not earn an unknown-label diagnostic.
    let src = "<?php\n#[\\Steins\\Effect('mutate.local')]\nfunction f(string $s): void { preg_match('/a/', $s, $m); }\n";
    assert_eq!(effects(src).len(), 0, "declared mutate.local is clean");
}

// ---- The arity leg ----------------------------------------------------------

#[test]
fn the_out_param_color_attaches_only_when_the_argument_is_supplied() {
    // Two calls to the same function; only the three-argument one writes.
    let with = "<?php\nfunction f(string $s): void { preg_match('/a/', $s, $m); }\n";
    let without = "<?php\nfunction f(string $s): void { preg_match('/a/', $s); }\n";
    assert_eq!(summary(with, "f").labels, vec!["mutate.local".to_owned()]);
    assert!(
        summary(without, "f").labels.is_empty(),
        "an omitted out-parameter is not written: {:?}",
        summary(without, "f").labels
    );
}

#[test]
fn a_named_argument_list_withholds_the_conditional_row() {
    // `preg_match(matches: $m, …)` supplies the out-parameter at no position at
    // all. Positional mapping is defeated, so the judgment is withheld rather
    // than guessed in either direction — the honest silence, not a false color.
    let src = "<?php\nfunction f(string $s): void { preg_match(pattern: '/a/', subject: $s, matches: $m); }\n";
    assert!(summary(src, "f").labels.is_empty(), "withheld, not guessed");
}

#[test]
fn the_always_by_ref_sort_family_always_attaches() {
    // `sort(array &$array)` has no arity condition to satisfy: argument 0 is
    // mandatory, so the color is unconditional in practice while still being
    // derived from the call.
    for f in ["sort", "rsort", "asort", "ksort", "array_pop", "array_shift", "reset"] {
        let src = format!("<?php\nfunction f(array $rows): void {{ {f}($rows); }}\n");
        assert_eq!(
            summary(&src, "f").labels,
            vec!["mutate.local".to_owned()],
            "{f} writes its array argument"
        );
    }
}

#[test]
fn preg_replace_counts_at_position_four_not_three() {
    // `preg_replace($pattern, $replacement, $subject, $limit, &$count)` — the
    // optional `$limit` sits between subject and count. A four-argument call
    // writes nothing; the five-argument one writes `$count`.
    let four = "<?php\nfunction f(string $s): string { return preg_replace('/a/', 'b', $s, 1); }\n";
    let five = "<?php\nfunction f(string $s): string { return preg_replace('/a/', 'b', $s, 1, $n); }\n";
    assert!(summary(four, "f").labels.is_empty(), "no count argument, no write");
    assert_eq!(summary(five, "f").labels, vec!["mutate.local".to_owned()]);
}

#[test]
fn str_replace_count_sits_at_position_three() {
    let three = "<?php\nfunction f(string $s): string { return str_replace('a', 'b', $s); }\n";
    let four = "<?php\nfunction f(string $s): string { return str_replace('a', 'b', $s, $n); }\n";
    assert!(summary(three, "f").labels.is_empty());
    assert_eq!(summary(four, "f").labels, vec!["mutate.local".to_owned()]);
}

// ---- The target leg ---------------------------------------------------------

#[test]
fn a_property_target_is_not_local_and_is_not_tolerated() {
    // The honesty case: the SAME builtin, the same arity, a different target.
    // Steins distinguishes them — a property-rooted by-ref write never claims
    // `mutate.local`, so the Pure tolerance cannot launder it.
    let src = "<?php\nclass C {\n    private array $m = [];\n    #[\\Steins\\Pure]\n    public function f(string $s): void { preg_match('/a/', $s, $this->m); }\n}\n";
    let d = one(src);
    assert!(d.message.contains("preg_match() has effect mutate"), "{}", d.message);
    assert!(!d.message.contains("mutate.local"), "a property target is not local: {}", d.message);
    assert!(d.message.contains("#[\\Steins\\Pure]"), "{}", d.message);
}

#[test]
fn a_superglobal_target_is_a_global_write() {
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(string $s): void { preg_match('/a/', $s, $_SESSION['m']); }\n";
    let d = one(src);
    assert!(d.message.contains("global.write"), "{}", d.message);
}

#[test]
fn an_offset_under_a_local_is_still_local() {
    // `sort($rows['a'])` writes into `$rows`, a frame binding — offsets are
    // transparent to the root classification.
    let src = "<?php\nfunction f(array $rows): void { sort($rows['a']); }\n";
    assert_eq!(summary(src, "f").labels, vec!["mutate.local".to_owned()]);
}

#[test]
fn a_by_ref_parameter_target_escapes_the_frame() {
    // `$rows` is an alias of the *caller's* binding, so writing it is
    // caller-observable — the one case that looks local and is not.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(array &$rows): void { sort($rows); }\n";
    let d = one(src);
    assert!(d.message.contains("has effect mutate,"), "{}", d.message);
    assert!(!d.message.contains("mutate.local"), "{}", d.message);
}

#[test]
fn an_aliased_frame_cannot_claim_locality() {
    // `global $rows;` makes the name the interpreter's. The give-up-list scan
    // (ADR-0001) is frame-wide, so any aliasing construct downgrades the claim.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(): void { global $rows; sort($rows); }\n";
    let d = one(src);
    assert!(d.message.contains("has effect mutate,"), "{}", d.message);
    assert!(!d.message.contains("mutate.local"), "{}", d.message);
}

// ---- Pure tolerance ---------------------------------------------------------

#[test]
fn pure_tolerates_preg_match_into_a_local() {
    // ADR-0063 §2.3, the headline: a pure function may scribble on its own
    // locals. This is the crying-wolf case 狼少年撲滅 exists to kill.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(string $s): bool { return (bool) preg_match('/a/', $s, $m); }\n";
    assert_eq!(effects(src).len(), 0, "preg_match into a local is pure");
    // The color is still *inferred* — tolerated, not invisible.
    assert_eq!(summary(src, "f").labels, vec!["mutate.local".to_owned()]);
}

#[test]
fn a_narrower_declaration_is_not_stricter_than_pure() {
    // Tolerance is monotone: `Pure` is the tightest envelope, so a label it
    // tolerates cannot exceed a wider declaration either.
    let src = "<?php\n#[\\Steins\\Effect('output')]\nfunction f(string $s): void { preg_match('/a/', $s, $m); echo $s; }\n";
    assert_eq!(effects(src).len(), 0, "output envelope also tolerates mutate.local");
}

#[test]
fn tolerance_does_not_extend_to_the_parent_label() {
    // `mutate` is not tolerated — only the frame-private child is.
    let src = "<?php\nclass C {\n    private array $m = [];\n    #[\\Steins\\Pure]\n    public function f(array $r): void { sort($this->m); }\n}\n";
    assert_eq!(effects(src).len(), 1, "an escaping mutation still exceeds Pure");
}

// ---- The composed axes ------------------------------------------------------

#[test]
fn shuffle_carries_both_its_own_color_and_the_by_ref_color() {
    // Two independent catalog axes on one call: `nondet.random` (unconditional)
    // and `mutate.local` (conditional on the argument).
    let src = "<?php\nfunction f(array $rows): void { shuffle($rows); }\n";
    let mut labels = summary(src, "f").labels;
    labels.sort();
    assert_eq!(labels, vec!["mutate.local".to_owned(), "nondet.random".to_owned()]);

    // Under Pure only the un-tolerated half is reported.
    let pure = "<?php\n#[\\Steins\\Pure]\nfunction f(array $rows): void { shuffle($rows); }\n";
    let d = one(pure);
    assert!(d.message.contains("nondet.random"), "{}", d.message);
}

// ---- P1's own-color leg, lit up by P2 ---------------------------------------

#[test]
fn pure_accepts_usort_over_a_local_with_a_pure_comparator() {
    // Before P2 the invoker's own-color leg was inert for `usort` — the catalog
    // had no color for it to contribute. Now it contributes `mutate.local`, and
    // the tolerance is what keeps this clean.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(array $rows): array {\n    usort($rows, function ($a, $b) { return $a <=> $b; });\n    return $rows;\n}\n";
    assert_eq!(effects(src).len(), 0, "pure comparator over a local array is clean");
    assert_eq!(summary(src, "f").labels, vec!["mutate.local".to_owned()]);
}

#[test]
fn usort_with_an_echoing_comparator_is_exceeded_by_echo_not_by_mutation() {
    // The join is real: the comparator's own color arrives, and it is the *only*
    // thing reported — the sort's own `mutate.local` stays tolerated.
    let src = "<?php\n#[\\Steins\\Pure]\nfunction f(array $rows): array {\n    usort($rows, function ($a, $b) { echo $a; return $a <=> $b; });\n    return $rows;\n}\n";
    let d = one(src);
    assert!(d.message.contains("output"), "named by the echo: {}", d.message);
    assert!(!d.message.contains("mutate"), "the sort's own write stays tolerated: {}", d.message);

    let mut labels = summary(src, "f").labels;
    labels.sort();
    assert_eq!(labels, vec!["mutate.local".to_owned(), "output".to_owned()]);
}

#[test]
fn array_walk_over_a_property_reports_the_escaping_write() {
    // `array_walk` is both an invoker and an out-param writer; over a property
    // its own color is the un-tolerated `mutate`.
    let src = "<?php\nclass C {\n    private array $rows = [];\n    #[\\Steins\\Pure]\n    public function f(): void { array_walk($this->rows, function ($v) { return $v; }); }\n}\n";
    let d = one(src);
    assert!(d.message.contains("array_walk() has effect mutate"), "{}", d.message);
}

// ---- The C9 pure-callable consumer inherits the tolerance -------------------

fn param_mismatches(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    check(&tree, &functions, "test.php").into_iter().filter(|d| d.id == PARAM_MISMATCH_ID).collect()
}

#[test]
fn a_closure_that_preg_matches_into_a_local_satisfies_pure_callable() {
    // One purity relation, two consumers: what `#[\Steins\Pure]` tolerates,
    // `pure-callable` must accept.
    const TAKES: &str = "<?php /** @param pure-callable $cb */ function takes($cb): void {}\n";
    let ok = format!(
        "{TAKES}takes(static function (string $s): bool {{ return (bool) preg_match('/a/', $s, $m); }});"
    );
    assert_eq!(param_mismatches(&ok).len(), 0, "local out-param write is pure: {:#?}", param_mismatches(&ok));

    // Control: the same call writing a superglobal is not.
    let bad = format!(
        "{TAKES}takes(static function (string $s): bool {{ return (bool) preg_match('/a/', $s, $_SESSION['m']); }});"
    );
    assert_eq!(param_mismatches(&bad).len(), 1, "a global write is not pure");
}
