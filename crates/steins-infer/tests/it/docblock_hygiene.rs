//! Docblock hygiene — the mechanics-layer anti-rot family (ADR-0078, issue #186).
//!
//! Six ids about annotations that drifted from the code they annotate:
//! `phpdoc.unparsable`, `phpdoc.stale-param`, `phpdoc.stale-var`,
//! `phpdoc.misplaced-var`, `phpdoc.throws-not-throwable`, `closure.unused-use`.
//! Every premise is textual, so each id is pinned by a **pair**: a minimal
//! firing fixture and its legal, silent counterpart.
//!
//! Two silences bind the whole family: a tag **outside the read set** is
//! never a finding however malformed (`steins_phpdoc` drops it first), and a
//! `@throws` naming a class the index cannot enumerate is silence, not a
//! finding (the absence-family condition).
//!
//! The `@var` legs reuse ADR-0073/0074's statement-adoption rule verbatim
//! (`SourceTree::stmt_docblock`), styled like `tests/inline_var_casts.rs`.

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

// phpdoc.unparsable — a read-set tag whose payload the parser rejects.

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

/// Type region is cut at first `$name`; by-ref's stray `&` is spelling, not rot.
#[test]
fn a_by_ref_param_spelling_is_not_unparsable() {
    silent("<?php\n/** @param int &$x */\nfunction f(int &$x): void {}\n", PHPDOC_UNPARSABLE_ID);
}

/// Same for the variadic spelling's trailing `...`.
#[test]
fn a_variadic_param_spelling_is_not_unparsable() {
    silent("<?php\n/** @param int ...$xs */\nfunction f(int ...$xs): void {}\n", PHPDOC_UNPARSABLE_ID);
}

/// Full callable type is parsed whole, not the scanner's truncated region.
#[test]
fn a_callable_type_with_named_parameters_parses() {
    silent(
        "<?php\n/** @param callable(CallbackInput $input): bool $callback */\nfunction f(callable $callback): void {}\n",
        PHPDOC_UNPARSABLE_ID,
    );
}

/// Wrapped array shape truncates to `array{` at one physical line — parser
/// limit, not rot.
#[test]
fn a_line_wrapped_array_shape_is_not_rot() {
    silent(
        "<?php\n/**\n * @param array{\n *   a: int,\n * } $x\n */\nfunction f(array $x): void {}\n",
        PHPDOC_UNPARSABLE_ID,
    );
}

/// Pins the read-set rule from the module doc directly.
#[test]
fn an_unknown_vendor_tag_never_reports() {
    let src = "<?php\n/**\n * @deprecated array{ <<< nonsense |\n * @psalm-param-out int|string| $x\n * @mycompany-thing }{\n */\nfunction f(int $x): void {}\n";
    silent(src, PHPDOC_UNPARSABLE_ID);
    silent(src, PHPDOC_STALE_PARAM_ID);
}

// phpdoc.stale-param — a `@param` naming a parameter the signature lacks.

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

#[test]
fn variadic_and_by_ref_parameters_exist() {
    silent("<?php\n/** @param int ...$xs */\nfunction f(int ...$xs): void {}\n", PHPDOC_STALE_PARAM_ID);
    silent("<?php\n/** @param int &$out */\nfunction f(int &$out): void {}\n", PHPDOC_STALE_PARAM_ID);
}

#[test]
fn a_param_tag_without_a_name_is_not_stale() {
    silent("<?php\n/** @param int */\nfunction f(int $n): void {}\n", PHPDOC_STALE_PARAM_ID);
}

/// Subject is the `$name` after TYPE, not inside a `callable(...)` payload —
/// misreading that false-flagged `$input` as missing (witness: phpunit Callback.php).
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

/// Unparsable payload has no locatable subject — that's phpdoc.unparsable's case alone.
#[test]
fn an_unparsable_payload_is_not_also_stale() {
    silent("<?php\n/** @param int|string| $gone */\nfunction f(int $n): void {}\n", PHPDOC_STALE_PARAM_ID);
}

/// **A multiline type never convicts.** Scanner cuts a wrapped `callable(…):
/// array{…}` at `array{`; `parse_type` BACKTRACKs the all-or-nothing
/// `callable(…)` form to the bare identifier, `consumed = 8`, param list
/// unconsumed — would misread `$params` as subject. Two guards pinned here:
/// bracket-unbalanced, and balanced-but-backtracked with no subject at depth 0.
#[test]
fn a_multiline_callable_type_convicts_nothing() {
    let src = "<?php\n/**\n * @phpstan-param callable(array<string,string> $params): array{\n *   subject:string,\n *   body:string\n * } $render\n */\nfunction f(callable $render): void {}\n";
    silent(src, PHPDOC_STALE_PARAM_ID);
    silent(src, PHPDOC_UNPARSABLE_ID);

    // Guard 2 alone: balanced callable, no return type, still backtracks to consumed=8.
    silent(
        "<?php\n/** @param callable(int $a) */\nfunction f(callable $cb): void {}\n",
        PHPDOC_STALE_PARAM_ID,
    );
}

#[test]
fn a_single_line_callable_with_a_stale_subject_still_fires() {
    let msg = one(
        "<?php\n/** @phpstan-param callable(array<string,string> $params): array{subject:string} $gone */\nfunction f(callable $render): void {}\n",
        PHPDOC_STALE_PARAM_ID,
    );
    assert_eq!(msg, "`@param $gone` names no parameter of f()");
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

// phpdoc.stale-var — an adopted `@var` naming a variable that exists NOWHERE.
//
// ADR-0073 §2/§4 (not PHPStan's `varTag.differentVariable`): only a
// referent-less name is rot; a mismatched-but-real name is legal.

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

/// ADR-0073 §2's own shape (witness: nikic/PHP-Parser test suite) — legal here.
#[test]
fn a_cast_of_a_variable_the_statement_reads_is_legal() {
    silent(
        "<?php\nfunction f(\\PhpParser\\Node\\Stmt\\Echo_ $echo): void {\n  /** @var \\PhpParser\\Node\\Stmt\\Echo_ $echo */\n  $dnumber = $echo->exprs[0];\n}\n",
        PHPDOC_STALE_VAR_ID,
    );
}

/// Several in-scope names in one docblock (witness: composer's AutoloadGenerator).
#[test]
fn a_multi_var_docblock_over_one_statement_is_legal() {
    silent(
        "<?php\nfunction f(string $vendorDir, string $baseDir): string {\n  /**\n   * @var string $vendorDir\n   * @var string $baseDir\n   */\n  $out = $vendorDir . $baseDir;\n  return $out;\n}\n",
        PHPDOC_STALE_VAR_ID,
    );
}

/// A name bound anywhere earlier in scope (assignment/foreach/closure use) is real.
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

/// Name-minting scope is silent throughout — same dam as `closure.unused-use`.
#[test]
fn a_name_minting_scope_dams_stale_var() {
    for prelude in ["extract($data);", "$k = 'a'; $$k = 1;", "eval('$q = 1;');"] {
        silent(
            &format!("<?php\nfunction f(array $data): void {{\n  {prelude}\n  /** @var int $whatever */\n  $other = 2;\n}}\n"),
            PHPDOC_STALE_VAR_ID,
        );
    }
}

#[test]
fn a_bare_var_tag_is_never_stale() {
    silent("<?php\nfunction f(): void {\n  /** @var int */\n  $x = 1;\n}\n", PHPDOC_STALE_VAR_ID);
}

/// Property-target spelling names a property, not the receiver (ADR-0073 §3 guard).
#[test]
fn a_property_target_var_tag_is_not_stale() {
    silent(
        "<?php\nclass C {\n  public function m(): void {\n    /** @var int $this->p */\n    $x = 1;\n  }\n}\n",
        PHPDOC_STALE_VAR_ID,
    );
}

/// A comment in the gap breaks adjacency (ADR-0073 §3) — nothing adopts the tag.
#[test]
fn a_comment_in_the_gap_breaks_the_adoption() {
    silent(
        "<?php\nfunction f(): void {\n  /** @var int $y */\n  // note\n  $x = 1;\n  $y = 2;\n}\n",
        PHPDOC_STALE_VAR_ID,
    );
}

/// `foreach`'s bound name is opaque to the trace — legal `@var`-over-loop, silent.
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

// phpdoc.misplaced-var — a `@var` nothing can adopt.

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

/// Property-`@var` position is legal, consumed by the declaration that follows.
#[test]
fn a_property_var_docblock_is_not_misplaced() {
    silent("<?php\nclass C {\n  /** @var int */\n  public $p = 0;\n}\n", PHPDOC_MISPLACED_VAR_ID);
}

/// A second docblock becomes the nearest trivium — the first adopts nothing.
#[test]
fn a_var_tag_shadowed_by_a_following_docblock_is_misplaced() {
    let ds = findings(
        "<?php\nfunction f(): void {\n  /** @var int $a */\n  /** @var int $b */\n  $b = 1;\n}\n",
        PHPDOC_MISPLACED_VAR_ID,
    );
    assert_eq!(ds.len(), 1, "only the shadowed docblock is misplaced: {ds:?}");
}

// phpdoc.throws-not-throwable — a `@throws` naming a proven non-Throwable.

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

/// The absence-family condition from the module doc, pinned directly.
#[test]
fn a_throws_on_an_unresolvable_class_is_silent() {
    silent(
        "<?php\n/** @throws \\Vendor\\Absent\\Boom */\nfunction f(): void {}\n",
        PHPDOC_THROWS_NOT_THROWABLE_ID,
    );
}

/// Ancestry leaving the known world is `Maybe`, and `Maybe` is silence.
#[test]
fn a_class_with_an_unknown_ancestor_is_silent() {
    silent(
        "<?php\nclass MyEx extends \\Vendor\\Absent\\Base {}\n/** @throws MyEx */\nfunction f(): void {}\n",
        PHPDOC_THROWS_NOT_THROWABLE_ID,
    );
}

// closure.unused-use — a by-value `use ($x)` the body never mentions.

#[test]
fn a_use_the_body_never_reads_reports() {
    let msg = one("<?php\n$f = function () use ($x) { return 1; };\n", CLOSURE_UNUSED_USE_ID);
    assert_eq!(msg, "`use ($x)` is never read in the closure body");
}

#[test]
fn a_use_the_body_reads_is_silent() {
    silent("<?php\n$f = function () use ($x) { return $x; };\n", CLOSURE_UNUSED_USE_ID);
}

/// By-ref `use (&$x)` is an out-channel — "never read" says nothing about it.
#[test]
fn a_by_ref_use_is_never_a_finding() {
    silent("<?php\n$f = function () use (&$x) { return 1; };\n", CLOSURE_UNUSED_USE_ID);
}

/// A name mentioned only inside a nested closure (body or its own `use`) counts.
#[test]
fn a_nested_closure_mention_counts_as_a_use() {
    silent(
        "<?php\n$f = function () use ($x) { return function () use ($x) { return $x; }; };\n",
        CLOSURE_UNUSED_USE_ID,
    );
    silent("<?php\n$f = function () use ($x) { return fn () => $x; };\n", CLOSURE_UNUSED_USE_ID);
}

/// A body that can consume a name without spelling it (`compact`/`extract`/
/// `get_defined_vars`/`$$x`/`eval`/`include`) silences the whole closure.
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

#[test]
fn an_interpolated_mention_counts() {
    silent("<?php\n$f = function () use ($x) { return \"a $x b\"; };\n", CLOSURE_UNUSED_USE_ID);
}
