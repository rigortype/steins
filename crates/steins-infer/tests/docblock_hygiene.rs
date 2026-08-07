//! Docblock hygiene — the mechanics-layer anti-rot family (ADR-0078, issue #186).
//!
//! Six ids about annotations that drifted from the code they annotate:
//! `phpdoc.unparsable`, `phpdoc.stale-param`, `phpdoc.stale-var`,
//! `phpdoc.misplaced-var`, `phpdoc.throws-not-throwable` and
//! `closure.unused-use`. Every premise is textual — the tag's subject either
//! exists or it does not — so each id is pinned here by a **pair**: one minimal
//! fixture that fires, and its legal counterpart that must stay silent.
//!
//! Two silences bind the whole family and are tested on their own:
//!
//! * A tag **outside the read set** (an unknown/vendor tag) is never a finding,
//!   however malformed — `steins_phpdoc` drops it before any check sees it.
//! * A `@throws` naming a class the index cannot enumerate is silence, not a
//!   finding (the absence-family condition).
//!
//! The `@var` legs reuse the ADR-0073/0074 statement-adoption rule verbatim
//! (`SourceTree::stmt_docblock`), so their fixtures follow the style of
//! `tests/inline_var_casts.rs`: an inline source string, the pure single-file
//! `check`, and an assertion on the ids that came back.

use steins_infer::{
    CLOSURE_UNUSED_USE_ID, Diagnostic, PHPDOC_MISPLACED_VAR_ID, PHPDOC_STALE_PARAM_ID,
    PHPDOC_STALE_VAR_ID, PHPDOC_THROWS_NOT_THROWABLE_ID, PHPDOC_UNPARSABLE_ID, check,
};
use steins_syntax::SourceTree;

/// Every finding of `id` the single-file check reports for `src`.
fn findings(src: &str, id: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php").into_iter().filter(|d| d.id == id).collect()
}

/// The one message `id` reports for `src` — asserts exactly one finding.
fn one(src: &str, id: &str) -> String {
    let ds = findings(src, id);
    assert_eq!(ds.len(), 1, "expected exactly one `{id}` for {src:?}, got {ds:?}");
    ds[0].message.clone()
}

/// Asserts `id` reports nothing for `src`.
fn silent(src: &str, id: &str) {
    let ds = findings(src, id);
    assert!(ds.is_empty(), "`{id}` must stay silent on {src:?}, got {ds:?}");
}

// ---------------------------------------------------------------------------
// phpdoc.unparsable — a read-set tag whose payload the parser rejects.
// ---------------------------------------------------------------------------

#[test]
fn a_trailing_union_bar_does_not_parse() {
    let msg = one("<?php\n/** @return int|string| */\nfunction f() {}\n", PHPDOC_UNPARSABLE_ID);
    assert!(msg.contains("@return int|string|"), "{msg}");
    assert!(msg.contains("declares nothing"), "{msg}");
}

#[test]
fn a_well_formed_type_parses() {
    silent("<?php\n/** @return array<int, string>|null */\nfunction f() {}\n", PHPDOC_UNPARSABLE_ID);
}

/// The docblock scanner cuts the type region at the first `$name`, so a by-ref
/// parameter leaves a stray `&` behind (`"int &"`). That is signature spelling,
/// not a broken type — trimmed before the parse, never a finding.
#[test]
fn a_by_ref_param_spelling_is_not_unparsable() {
    silent("<?php\n/** @param int &$x */\nfunction f(int &$x): void {}\n", PHPDOC_UNPARSABLE_ID);
}

/// Same for the variadic spelling's trailing `...`.
#[test]
fn a_variadic_param_spelling_is_not_unparsable() {
    silent("<?php\n/** @param int ...$xs */\nfunction f(int ...$xs): void {}\n", PHPDOC_UNPARSABLE_ID);
}

/// A callable type carrying its own parameter names parses whole — the payload,
/// not the scanner's `$`-truncated type region, is what the parser is handed
/// (sebastianbergmann/phpunit `Framework/Constraint/Callback.php`).
#[test]
fn a_callable_type_with_named_parameters_parses() {
    silent(
        "<?php\n/** @param callable(CallbackInput $input): bool $callback */\nfunction f(callable $callback): void {}\n",
        PHPDOC_UNPARSABLE_ID,
    );
}

/// The scanner reads one physical line, so a wrapped array shape arrives
/// truncated (`"array{"`). An unbalanced payload is a parser limitation, never
/// rot — documented silence, not an omitted check.
#[test]
fn a_line_wrapped_array_shape_is_not_rot() {
    silent(
        "<?php\n/**\n * @param array{\n *   a: int,\n * } $x\n */\nfunction f(array $x): void {}\n",
        PHPDOC_UNPARSABLE_ID,
    );
}

/// The bounded-tag-set discipline: Steins reads a bounded vocabulary and drops
/// everything else, so an unknown/vendor tag is never a finding however malformed.
#[test]
fn an_unknown_vendor_tag_never_reports() {
    let src = "<?php\n/**\n * @deprecated array{ <<< nonsense |\n * @psalm-param-out int|string| $x\n * @mycompany-thing }{\n */\nfunction f(int $x): void {}\n";
    silent(src, PHPDOC_UNPARSABLE_ID);
    silent(src, PHPDOC_STALE_PARAM_ID);
}

// ---------------------------------------------------------------------------
// phpdoc.stale-param — a `@param` naming a parameter the signature lacks.
// ---------------------------------------------------------------------------

#[test]
fn a_param_tag_naming_no_parameter_is_stale() {
    let msg =
        one("<?php\n/** @param int $count */\nfunction f(int $n): void {}\n", PHPDOC_STALE_PARAM_ID);
    assert_eq!(msg, "`@param $count` names no parameter of f()");
}

#[test]
fn a_param_tag_naming_a_real_parameter_is_silent() {
    silent("<?php\n/** @param int $n */\nfunction f(int $n): void {}\n", PHPDOC_STALE_PARAM_ID);
}

/// Variadic and by-ref spellings name a real parameter — both count as existing.
#[test]
fn variadic_and_by_ref_parameters_exist() {
    silent("<?php\n/** @param int ...$xs */\nfunction f(int ...$xs): void {}\n", PHPDOC_STALE_PARAM_ID);
    silent("<?php\n/** @param int &$out */\nfunction f(int &$out): void {}\n", PHPDOC_STALE_PARAM_ID);
}

/// A `@param` with no name token names nothing — not this finding.
#[test]
fn a_param_tag_without_a_name_is_not_stale() {
    silent("<?php\n/** @param int */\nfunction f(int $n): void {}\n", PHPDOC_STALE_PARAM_ID);
}

/// **The subject is the `$name` after the TYPE, not the first `$name` in the
/// payload.** A `callable(...)` type carries its own parameter names, and the
/// docblock scanner's type region stops at the first of them — reading that as the
/// subject reported `$input` as a missing parameter of a signature that never had
/// one (sebastianbergmann/phpunit `Framework/Constraint/Callback.php`).
#[test]
fn a_parameter_name_inside_a_callable_type_is_not_the_subject() {
    silent(
        "<?php\n/** @param callable(CallbackInput $input): bool $callback */\nfunction f(callable $callback): void {}\n",
        PHPDOC_STALE_PARAM_ID,
    );
    silent(
        "<?php\n/** @param \\Closure(Foo $f, Bar $b): void $fn */\nfunction f(\\Closure $fn): void {}\n",
        PHPDOC_STALE_PARAM_ID,
    );
    silent(
        "<?php\n/** @param list<callable(Foo $f): void> $callbacks */\nfunction f(array $callbacks): void {}\n",
        PHPDOC_STALE_PARAM_ID,
    );
    // ...and the real subject is still judged: here it IS stale.
    let msg = one(
        "<?php\n/** @param callable(CallbackInput $input): bool $gone */\nfunction f(callable $callback): void {}\n",
        PHPDOC_STALE_PARAM_ID,
    );
    assert_eq!(msg, "`@param $gone` names no parameter of f()");
}

/// A payload the parser rejects has no locatable subject, so staleness is not
/// asked — that case is `phpdoc.unparsable`'s alone.
#[test]
fn an_unparsable_payload_is_not_also_stale() {
    silent("<?php\n/** @param int|string| $gone */\nfunction f(int $n): void {}\n", PHPDOC_STALE_PARAM_ID);
}

#[test]
fn a_method_param_tag_is_checked_against_its_own_signature() {
    let msg = one(
        "<?php\nclass C {\n  /** @param string $old */\n  public function m(string $name): void {}\n}\n",
        PHPDOC_STALE_PARAM_ID,
    );
    assert_eq!(msg, "`@param $old` names no parameter of C::m()");
}

#[test]
fn a_closure_param_tag_is_checked_too() {
    let msg = one(
        "<?php\n/** @param int $gone */\n$f = function (int $n): void {};\n",
        PHPDOC_STALE_PARAM_ID,
    );
    assert_eq!(msg, "`@param $gone` names no parameter of the closure");
}

// ---------------------------------------------------------------------------
// phpdoc.stale-var — an adopted `@var` naming a variable that exists NOWHERE.
//
// The claim is ADR-0073's, not PHPStan's: §2 makes the cast re-declare the
// variable the tag NAMES, whatever the statement below it binds, and §4 defers the
// assignment form as a silence. So "the tag names a different variable than the
// statement assigns" — PHPStan's `varTag.differentVariable` — is legal here, and
// only a name with no referent at all is rot.
// ---------------------------------------------------------------------------

#[test]
fn a_var_tag_naming_a_variable_that_exists_nowhere_is_stale() {
    let msg = one(
        "<?php\nfunction f(): void {\n  /** @var int $y */\n  $x = 1;\n}\n",
        PHPDOC_STALE_VAR_ID,
    );
    assert_eq!(
        msg,
        "`@var $y` names a variable that appears nowhere before this statement in the scope"
    );
}

/// The typo shape the narrowed check exists for.
#[test]
fn a_misspelled_variable_name_is_stale() {
    let src = "<?php\nfunction f(\\PhpParser\\Node\\Stmt\\Echo_ $echo): void {\n  /** @var \\PhpParser\\Node\\Stmt\\Echo_ $ecoh */\n  $dnumber = $echo->exprs[0];\n}\n";
    let msg = one(src, PHPDOC_STALE_VAR_ID);
    assert!(msg.contains("`@var $ecoh`"), "{msg}");
}

#[test]
fn a_var_tag_naming_the_bound_variable_is_silent() {
    silent("<?php\nfunction f(): void {\n  /** @var int $x */\n  $x = 1;\n}\n", PHPDOC_STALE_VAR_ID);
}

/// **ADR-0073 §2's own shape**: the cast re-declares an already-bound variable the
/// statement merely *reads*. `nikic/PHP-Parser`'s test suite spells it this way,
/// and PHPStan calls it `varTag.differentVariable` — we do not.
#[test]
fn a_cast_of_a_variable_the_statement_reads_is_legal() {
    silent(
        "<?php\nfunction f(\\PhpParser\\Node\\Stmt\\Echo_ $echo): void {\n  /** @var \\PhpParser\\Node\\Stmt\\Echo_ $echo */\n  $dnumber = $echo->exprs[0];\n}\n",
        PHPDOC_STALE_VAR_ID,
    );
}

/// A multi-`@var` docblock naming several in-scope variables at once
/// (`composer/composer`'s `AutoloadGenerator`). Every name exists; nothing is rot.
#[test]
fn a_multi_var_docblock_over_one_statement_is_legal() {
    silent(
        "<?php\nfunction f(string $vendorDir, string $baseDir): string {\n  /**\n   * @var string $vendorDir\n   * @var string $baseDir\n   */\n  $out = $vendorDir . $baseDir;\n  return $out;\n}\n",
        PHPDOC_STALE_VAR_ID,
    );
}

/// A name bound anywhere earlier in the scope — by an assignment, a `foreach`, a
/// closure capture — is a real variable, whatever the statement below the tag does.
#[test]
fn a_name_bound_earlier_in_the_scope_is_never_stale() {
    for prelude in [
        "$item = 1;",
        "foreach ([1] as $item) { echo $item; }",
        "$f = function () use ($item) { return $item; };",
    ] {
        silent(
            &format!("<?php\nfunction f(): void {{\n  {prelude}\n  /** @var int $item */\n  $other = 2;\n}}\n"),
            PHPDOC_STALE_VAR_ID,
        );
    }
}

/// A scope that can mint names is silent throughout — the same dam
/// `closure.unused-use` applies.
#[test]
fn a_name_minting_scope_dams_stale_var() {
    for prelude in ["extract($data);", "$k = 'a'; $$k = 1;", "eval('$q = 1;');"] {
        silent(
            &format!("<?php\nfunction f(array $data): void {{\n  {prelude}\n  /** @var int $whatever */\n  $other = 2;\n}}\n"),
            PHPDOC_STALE_VAR_ID,
        );
    }
}

/// A bare `@var T` speaks about the statement's own binding — never stale.
#[test]
fn a_bare_var_tag_is_never_stale() {
    silent("<?php\nfunction f(): void {\n  /** @var int */\n  $x = 1;\n}\n", PHPDOC_STALE_VAR_ID);
}

/// The property-target spelling speaks about a property, not the receiver — the
/// ADR-0073 §3 guard, honored here exactly as the cast lane honors it.
#[test]
fn a_property_target_var_tag_is_not_stale() {
    silent(
        "<?php\nclass C {\n  public function m(): void {\n    /** @var int $this->p */\n    $x = 1;\n  }\n}\n",
        PHPDOC_STALE_VAR_ID,
    );
}

/// A comment in the gap breaks the adjacency (ADR-0073 §3), so no statement
/// adopts the tag and the different-variable question is never asked.
#[test]
fn a_comment_in_the_gap_breaks_the_adoption() {
    silent(
        "<?php\nfunction f(): void {\n  /** @var int $y */\n  // note\n  $x = 1;\n  $y = 2;\n}\n",
        PHPDOC_STALE_VAR_ID,
    );
}

/// `foreach` binds its value variable through a construct the trace keeps opaque,
/// so the statement's bound name is not a syntactic fact — silence, the common
/// and entirely legal `@var` over a loop.
#[test]
fn a_var_tag_over_a_foreach_is_silent() {
    silent(
        "<?php\nfunction f(array $xs): void {\n  /** @var int $item */\n  foreach ($xs as $item) { echo $item; }\n}\n",
        PHPDOC_STALE_VAR_ID,
    );
}

#[test]
fn a_stale_var_inside_a_branch_still_reports() {
    let msg = one(
        "<?php\nfunction f(bool $c): void {\n  if ($c) {\n    /** @var int $nowhere */\n    $x = 1;\n  }\n}\n",
        PHPDOC_STALE_VAR_ID,
    );
    assert!(msg.contains("`@var $nowhere`"), "{msg}");
}

// ---------------------------------------------------------------------------
// phpdoc.misplaced-var — a `@var` nothing can adopt.
// ---------------------------------------------------------------------------

#[test]
fn a_var_tag_with_nothing_following_is_misplaced() {
    let msg =
        one("<?php\nfunction f(): void {\n  $x = 1;\n  /** @var int $x */\n}\n", PHPDOC_MISPLACED_VAR_ID);
    assert_eq!(msg, "`@var` sits where nothing adopts it — no declaration or statement follows");
}

#[test]
fn a_var_tag_a_statement_adopts_is_placed() {
    silent(
        "<?php\nfunction f(): void {\n  /** @var int $x */\n  $x = 1;\n}\n",
        PHPDOC_MISPLACED_VAR_ID,
    );
}

/// The property-`@var` position is legal and consumed elsewhere (the property
/// declaration follows it) — never misplaced.
#[test]
fn a_property_var_docblock_is_not_misplaced() {
    silent("<?php\nclass C {\n  /** @var int */\n  public $p = 0;\n}\n", PHPDOC_MISPLACED_VAR_ID);
}

/// A second docblock takes over as the nearest preceding trivium, so the first
/// one adopts nothing at all.
#[test]
fn a_var_tag_shadowed_by_a_following_docblock_is_misplaced() {
    let ds = findings(
        "<?php\nfunction f(): void {\n  /** @var int $a */\n  /** @var int $b */\n  $b = 1;\n}\n",
        PHPDOC_MISPLACED_VAR_ID,
    );
    assert_eq!(ds.len(), 1, "only the shadowed docblock is misplaced: {ds:?}");
}

// ---------------------------------------------------------------------------
// phpdoc.throws-not-throwable — a `@throws` naming a proven non-Throwable.
// ---------------------------------------------------------------------------

#[test]
fn a_throws_naming_a_plain_class_reports() {
    let msg = one(
        "<?php\nclass Payload {}\n/** @throws Payload */\nfunction f(): void {}\n",
        PHPDOC_THROWS_NOT_THROWABLE_ID,
    );
    assert!(msg.contains("@throws Payload"), "{msg}");
    assert!(msg.contains("not a Throwable"), "{msg}");
}

#[test]
fn a_throws_naming_a_real_exception_is_silent() {
    silent(
        "<?php\nclass MyEx extends \\RuntimeException {}\n/** @throws MyEx */\nfunction f(): void {}\n",
        PHPDOC_THROWS_NOT_THROWABLE_ID,
    );
    silent(
        "<?php\n/** @throws \\RuntimeException */\nfunction f(): void {}\n",
        PHPDOC_THROWS_NOT_THROWABLE_ID,
    );
}

/// The absence-family condition: a class the index cannot enumerate has no
/// verdict, so `@throws` on it is silence — never a finding.
#[test]
fn a_throws_on_an_unresolvable_class_is_silent() {
    silent(
        "<?php\n/** @throws \\Vendor\\Absent\\Boom */\nfunction f(): void {}\n",
        PHPDOC_THROWS_NOT_THROWABLE_ID,
    );
}

/// A class whose ancestry leaves the known world is `Maybe`, and `Maybe` is
/// silence — non-membership is provable only under a closed hierarchy.
#[test]
fn a_class_with_an_unknown_ancestor_is_silent() {
    silent(
        "<?php\nclass MyEx extends \\Vendor\\Absent\\Base {}\n/** @throws MyEx */\nfunction f(): void {}\n",
        PHPDOC_THROWS_NOT_THROWABLE_ID,
    );
}

// ---------------------------------------------------------------------------
// closure.unused-use — a by-value `use ($x)` the body never mentions.
// ---------------------------------------------------------------------------

#[test]
fn a_use_the_body_never_reads_reports() {
    let msg = one("<?php\n$f = function () use ($x) { return 1; };\n", CLOSURE_UNUSED_USE_ID);
    assert_eq!(msg, "`use ($x)` is never read in the closure body");
}

#[test]
fn a_use_the_body_reads_is_silent() {
    silent("<?php\n$f = function () use ($x) { return $x; };\n", CLOSURE_UNUSED_USE_ID);
}

/// A by-ref `use (&$x)` is an out-channel: the closure writes through it, so
/// "never read" says nothing about it. Never a finding.
#[test]
fn a_by_ref_use_is_never_a_finding() {
    silent("<?php\n$f = function () use (&$x) { return 1; };\n", CLOSURE_UNUSED_USE_ID);
}

/// A name mentioned only by a nested closure — in its body *or* in its own `use`
/// clause — is used by the outer capture's lights.
#[test]
fn a_nested_closure_mention_counts_as_a_use() {
    silent(
        "<?php\n$f = function () use ($x) { return function () use ($x) { return $x; }; };\n",
        CLOSURE_UNUSED_USE_ID,
    );
    silent("<?php\n$f = function () use ($x) { return fn () => $x; };\n", CLOSURE_UNUSED_USE_ID);
}

/// The scope-local dam: a body that can consume a name without spelling it
/// (`compact`/`extract`/`get_defined_vars`/`$$x`/`eval`/`include`) silences the
/// whole closure.
#[test]
fn a_name_minting_body_dams_the_closure() {
    for body in [
        "return compact('x');",
        "extract($vars); return 1;",
        "return $$name;",
        "eval('1'); return 1;",
        "include 'a.php'; return 1;",
        "return get_defined_vars();",
    ] {
        silent(&format!("<?php\n$f = function () use ($x) {{ {body} }};\n"), CLOSURE_UNUSED_USE_ID);
    }
}

/// A string-interpolated or heredoc mention is a mention.
#[test]
fn an_interpolated_mention_counts() {
    silent("<?php\n$f = function () use ($x) { return \"a $x b\"; };\n", CLOSURE_UNUSED_USE_ID);
}
