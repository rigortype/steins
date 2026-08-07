//! `variable.undefined` (ADR-0078, issue #194): a read of a name its scope
//! **never binds**, by any binding form, anywhere in the scope.
//!
//! PHP's own consequence (`php -r`-witnessed, 8.5.9):
//! `Warning: Undefined variable $x`, and the read evaluates to `null`.
//!
//! The premise is deliberately weaker than PHP's: ordering and branching are
//! ignored, so one binding form anywhere in the scope is silence. A read that
//! precedes its only assignment therefore belongs to the `variable.maybe-undefined`
//! sibling, which waits on the reachability foundation (issue #199) — pinned below.
//!
//! No sidecar and no folder dependency: the check reads a lowering-computed
//! syntactic fact, so every fixture uses the sound-subset [`NoFold`] folder. The two
//! gates that DO apply are the `warning-handler` posture (ADR-0049 §7) and the
//! ADR-0077 out-parameter subtraction, both exercised below.

use steins_infer::{Diagnostic, NoFold, VARIABLE_UNDEFINED_ID, check_full};
use steins_syntax::SourceTree;

fn diags(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_full(&tree, "test.php", &mut NoFold, true)
        .into_iter()
        .filter(|d| d.id == VARIABLE_UNDEFINED_ID)
        .collect()
}

/// Assert the fixture is silent for this id.
fn silent(src: &str) {
    let d = diags(src);
    assert!(d.is_empty(), "expected silence, got: {d:#?}");
}

/// Assert exactly one finding, naming `$name`.
fn fires(src: &str, name: &str) -> Diagnostic {
    let d = diags(src);
    assert_eq!(d.len(), 1, "expected exactly one finding: {d:#?}");
    assert!(d[0].message.contains(&format!("${name}")), "{}", d[0].message);
    d[0].clone()
}

// ---------------------------------------------------------------------------
// Firing fixtures.
// ---------------------------------------------------------------------------

#[test]
fn fires_on_a_plain_read_of_a_never_bound_name() {
    let d = fires("<?php\nfunction f(): int {\n    return $count;\n}\n", "count");
    assert_eq!(d.line, 3, "the read's own line: {d:#?}");
    assert!(
        d.message.contains("PHP warns \"Undefined variable $count\""),
        "the message carries PHP's own sentence: {}",
        d.message
    );
    assert!(d.message.contains("evaluates to null"), "{}", d.message);
}

#[test]
fn fires_on_a_typo_beside_the_similar_name_it_meant() {
    // The shape the id exists for: `$reuslt` is bound nowhere, `$result` is.
    let d = fires(
        "<?php\nfunction f(): int {\n    $result = 1;\n    return $reuslt;\n}\n",
        "reuslt",
    );
    assert_eq!(d.line, 4, "{d:#?}");
}

#[test]
fn fires_inside_a_method_body() {
    fires(
        "<?php\nclass C {\n    public function m(): int {\n        return $missing;\n    }\n}\n",
        "missing",
    );
}

#[test]
fn fires_inside_a_closure_body() {
    // A closure does NOT auto-capture, so an outer name it never `use`s is unbound
    // in its own frame.
    fires(
        "<?php\nfunction f(): callable {\n    $outer = 1;\n    return function () {\n        return $outer;\n    };\n}\n",
        "outer",
    );
}

#[test]
fn fires_on_a_heredoc_interpolation() {
    // Witnessed at 8.5.9: interpolation of an unbound name warns and interpolates
    // the empty string.
    let d = fires(
        "<?php\nfunction f(): string {\n    return <<<TXT\n        val=$hnope\n        TXT;\n}\n",
        "hnope",
    );
    assert_eq!(d.line, 4, "{d:#?}");
}

#[test]
fn fires_on_a_double_quoted_interpolation() {
    fires("<?php\nfunction f(): string {\n    return \"val=$nope\";\n}\n", "nope");
}

#[test]
fn fires_on_a_foreach_subject() {
    // The subject is a read; the loop's own bindings are `$v`, not `$rows`.
    fires(
        "<?php\nfunction f(): void {\n    foreach ($rows as $v) {\n        echo $v;\n    }\n}\n",
        "rows",
    );
}

#[test]
fn fires_on_the_index_of_an_offset_write() {
    // `$out[...] = 1` binds `$out`; the INDEX is an ordinary read.
    fires("<?php\nfunction f(): void {\n    $out[$key] = 1;\n}\n", "key");
}

#[test]
fn fires_on_a_by_value_use_clause_naming_an_unbound_enclosing_name() {
    // Witnessed at 8.5.9: `use ($un)` on an unbound name warns AT THE USE CLAUSE.
    // The read belongs to the ENCLOSING scope, which is where it is reported.
    let d = fires(
        "<?php\nfunction f(): callable {\n    return function () use ($un) {\n        return 1;\n    };\n}\n",
        "un",
    );
    assert_eq!(d.line, 3, "the use clause's own line: {d:#?}");
}

#[test]
fn fires_once_per_read_site() {
    let d = diags("<?php\nfunction f(): int {\n    return $a + $a;\n}\n");
    assert_eq!(d.len(), 2, "two read sites, two findings: {d:#?}");
}

// ---------------------------------------------------------------------------
// Binding forms — each one a pinned silence.
// ---------------------------------------------------------------------------

#[test]
fn a_parameter_binds() {
    silent("<?php\nfunction f(int $x): int {\n    return $x;\n}\n");
}

#[test]
fn a_promoted_constructor_property_binds() {
    silent(
        "<?php\nclass C {\n    public function __construct(private int $x) {\n        echo $x;\n    }\n}\n",
    );
}

#[test]
fn a_variadic_parameter_binds() {
    silent("<?php\nfunction f(int ...$rest): array {\n    return $rest;\n}\n");
}

#[test]
fn a_by_ref_parameter_binds() {
    silent("<?php\nfunction f(array &$out): void {\n    $out[] = 1;\n}\n");
}

#[test]
fn a_plain_assignment_binds() {
    silent("<?php\nfunction f(): int {\n    $x = 1;\n    return $x;\n}\n");
}

#[test]
fn a_compound_assignment_binds() {
    // PHP itself warns here (`$n .= 'x'` reads `$n` first), but an assignment
    // target anywhere in the scope is this id's silence: the ordering claim is
    // `variable.maybe-undefined`'s (issue #199).
    silent("<?php\nfunction f(): string {\n    $n .= 'x';\n    return $n;\n}\n");
}

#[test]
fn an_increment_binds() {
    silent("<?php\nfunction f(): int {\n    $c++;\n    return $c;\n}\n");
    silent("<?php\nfunction f(): int {\n    ++$d;\n    return $d;\n}\n");
}

#[test]
fn a_null_coalesce_assignment_binds() {
    silent("<?php\nfunction f(): int {\n    $x ??= 1;\n    return $x;\n}\n");
}

#[test]
fn an_offset_write_binds_its_base() {
    // Witnessed at 8.5.9: `$arr['k'] = 1` auto-vivifies `$arr` with no warning.
    silent("<?php\nfunction f(): array {\n    $arr['k'] = 1;\n    return $arr;\n}\n");
    silent("<?php\nfunction f(): array {\n    $arr[] = 1;\n    return $arr;\n}\n");
}

#[test]
fn a_property_write_binds_its_base() {
    silent("<?php\nfunction f(): object {\n    $o->p = 1;\n    return $o;\n}\n");
}

#[test]
fn array_destructuring_binds_every_target() {
    silent("<?php\nfunction f(array $in): int {\n    [$a, $b] = $in;\n    return $a + $b;\n}\n");
}

#[test]
fn list_destructuring_binds_every_target() {
    silent(
        "<?php\nfunction f(array $in): int {\n    list($a, $b) = $in;\n    return $a + $b;\n}\n",
    );
}

#[test]
fn a_partial_list_binds_the_targets_it_writes() {
    // Witnessed at 8.5.9: `[$a, $b] = [1]` warns "Undefined array key 1" — an
    // offset finding, not this one — and `$b` IS bound, to null.
    silent("<?php\nfunction f(): void {\n    [$a, $b] = [1];\n    echo $a, $b;\n}\n");
}

#[test]
fn a_skipped_list_slot_does_not_disturb_its_neighbours() {
    silent("<?php\nfunction f(array $in): mixed {\n    [, $b] = $in;\n    return $b;\n}\n");
}

#[test]
fn nested_destructuring_binds_the_inner_targets() {
    silent("<?php\nfunction f(array $in): int {\n    [[$a], $b] = $in;\n    return $a + $b;\n}\n");
}

#[test]
fn keyed_destructuring_binds_its_values() {
    silent(
        "<?php\nfunction f(array $in): int {\n    ['a' => $a, 'b' => $b] = $in;\n    return $a + $b;\n}\n",
    );
}

#[test]
fn a_global_declaration_binds() {
    // Witnessed at 8.5.9: `global $neverset; return $neverset;` returns null with
    // no warning — the declaration itself creates the binding.
    silent("<?php\nfunction f(): mixed {\n    global $g;\n    return $g;\n}\n");
}

#[test]
fn a_static_declaration_binds() {
    silent("<?php\nfunction f(): mixed {\n    static $s;\n    return $s;\n}\n");
    silent("<?php\nfunction f(): int {\n    static $n = 0;\n    return $n;\n}\n");
}

#[test]
fn a_by_value_closure_use_binds_inside_the_closure() {
    silent(
        "<?php\nfunction f(int $v): callable {\n    return function () use ($v) {\n        return $v;\n    };\n}\n",
    );
}

#[test]
fn a_by_ref_closure_use_binds_inside_the_closure() {
    silent(
        "<?php\nfunction f(): callable {\n    $r = 1;\n    return function () use (&$r) {\n        return $r;\n    };\n}\n",
    );
}

#[test]
fn a_by_ref_closure_use_binds_in_the_enclosing_scope_too() {
    // Witnessed at 8.5.9: `use (&$r2)` on an unbound name is SILENT and the name
    // reads back null afterwards — the clause creates the binding.
    silent(
        "<?php\nfunction f(): mixed {\n    $c = function () use (&$r) {\n        $r = 5;\n    };\n    $c();\n    return $r;\n}\n",
    );
}

#[test]
fn a_catch_binding_binds() {
    silent(
        "<?php\nfunction f(): string {\n    try {\n        g();\n    } catch (RuntimeException $e) {\n        return $e->getMessage();\n    }\n    return '';\n}\nfunction g(): void {}\n",
    );
}

#[test]
fn a_foreach_value_binding_binds() {
    silent(
        "<?php\nfunction f(array $rows): void {\n    foreach ($rows as $v) {\n        echo $v;\n    }\n}\n",
    );
}

#[test]
fn a_foreach_key_binding_binds() {
    silent(
        "<?php\nfunction f(array $rows): void {\n    foreach ($rows as $k => $v) {\n        echo $k, $v;\n    }\n}\n",
    );
}

#[test]
fn a_foreach_by_ref_value_binding_binds() {
    silent(
        "<?php\nfunction f(array $rows): void {\n    foreach ($rows as &$v) {\n        $v = 1;\n    }\n    echo $v;\n}\n",
    );
}

#[test]
fn a_foreach_destructuring_binding_binds() {
    silent(
        "<?php\nfunction f(array $rows): void {\n    foreach ($rows as [$a, $b]) {\n        echo $a, $b;\n    }\n}\n",
    );
}

#[test]
fn a_reference_assignment_binds_both_sides() {
    // Witnessed at 8.5.9: `$a = &$b;` binds `$b` as well as `$a`, silently.
    silent("<?php\nfunction f(): array {\n    $a = &$b;\n    $b = 9;\n    return [$a, $b];\n}\n");
}

// ---------------------------------------------------------------------------
// The out-parameter binding form (ADR-0077) — the checker-side subtraction.
// ---------------------------------------------------------------------------

#[test]
fn a_builtin_out_parameter_binds() {
    // Witnessed at 8.5.9: `preg_match('/a/', 'a', $m)` binds `$m`. The catalog's
    // `out_params` row for `preg_match` position 2 is what says so.
    silent(
        "<?php\nfunction f(string $s): array {\n    preg_match('/a/', $s, $m);\n    return $m;\n}\n",
    );
}

#[test]
fn a_userland_out_parameter_binds() {
    // Witnessed at 8.5.9: `fill($z)` on `function fill(&$out)` binds `$z`. Here the
    // cross-file index — not the catalog — carries the `&$out` declaration.
    silent(
        "<?php\nfunction fill(mixed &$out): void {\n    $out = 42;\n}\nfunction f(): mixed {\n    fill($z);\n    return $z;\n}\n",
    );
}

#[test]
fn a_sort_style_out_parameter_binds() {
    silent("<?php\nfunction f(): array {\n    sort($items);\n    return $items;\n}\n");
}

#[test]
fn a_method_argument_binds_because_no_callee_can_be_named() {
    silent(
        "<?php\nfunction f(object $o): mixed {\n    $o->fill($z);\n    return $z;\n}\n",
    );
}

#[test]
fn a_static_call_argument_binds() {
    silent("<?php\nfunction f(): mixed {\n    C::fill($z);\n    return $z;\n}\n");
}

#[test]
fn a_dynamic_call_argument_binds() {
    silent("<?php\nfunction f(callable $c): mixed {\n    $c($z);\n    return $z;\n}\n");
}

#[test]
fn a_constructor_argument_binds() {
    silent("<?php\nfunction f(): mixed {\n    new C($z);\n    return $z;\n}\n");
}

#[test]
fn a_named_argument_binds() {
    silent("<?php\nfunction f(): mixed {\n    g(out: $z);\n    return $z;\n}\nfunction g(mixed $out): void {}\n");
}

#[test]
fn an_unresolvable_function_argument_binds() {
    // `arg_is_by_value` refuses for every uncertainty, and refusal is silence here.
    silent(
        "<?php\nfunction f(): mixed {\n    some_extension_fn($z);\n    return $z;\n}\n",
    );
}

#[test]
fn an_out_parameter_inside_an_error_control_guard_still_binds() {
    // symfony/console `Terminal.php`: `@proc_open($cmd, $spec, $pipes, …)` binds
    // `$pipes` in PHP exactly as it would without the `@`, and the LATER reads of
    // `$pipes` must be silent. The guard withholds the argument occurrence from the
    // read list, so the binding is collected on its own terms — deriving it from
    // the reads reported all three `$pipes` lines.
    silent(
        "<?php\nfunction f(string $cmd, array $spec): string {\n    if (!$p = @proc_open($cmd, $spec, $pipes, null, null, [])) {\n        return '';\n    }\n    $info = stream_get_contents($pipes[1]);\n    fclose($pipes[1]);\n    fclose($pipes[2]);\n    return $info;\n}\n",
    );
}

#[test]
fn an_out_parameter_inside_an_isset_guard_still_binds() {
    silent(
        "<?php\nfunction f(string $p, string $s): array {\n    if (empty(preg_match($p, $s, $m))) {\n        return [];\n    }\n    return $m;\n}\n",
    );
}

#[test]
fn a_certified_by_value_builtin_argument_still_fires() {
    // The subtraction is not a blanket amnesty for call arguments: `strlen`'s
    // parameter is certified by value, so the read stands.
    fires("<?php\nfunction f(): int {\n    return strlen($s);\n}\n", "s");
}

#[test]
fn a_by_value_userland_argument_still_fires() {
    fires(
        "<?php\nfunction g(string $s): void {}\nfunction f(): void {\n    g($missing);\n}\n",
        "missing",
    );
}

// ---------------------------------------------------------------------------
// Scope dams — each one silences the WHOLE scope.
// ---------------------------------------------------------------------------

#[test]
fn extract_dams_the_scope() {
    // Witnessed at 8.5.9: `extract($d)` mints `$minted` from an array key.
    silent(
        "<?php\nfunction f(array $d): mixed {\n    extract($d);\n    return $minted;\n}\n",
    );
}

#[test]
fn compact_dams_the_scope() {
    // `compact` only READS names, and answers an undefined one with its OWN warning
    // (`compact(): Undefined variable $nope`, witnessed at 8.5.9), so it cannot
    // un-prove a binding — but it dams anyway, matching `closure.unused-use`.
    silent("<?php\nfunction f(): mixed {\n    $a = compact('x');\n    return $whatever;\n}\n");
}

#[test]
fn get_defined_vars_dams_the_scope() {
    silent(
        "<?php\nfunction f(): mixed {\n    $a = get_defined_vars();\n    return $whatever;\n}\n",
    );
}

#[test]
fn a_variable_variable_write_dams_the_scope() {
    // Witnessed at 8.5.9: `$n = 'dyn'; $$n = 'ok';` mints `$dyn`.
    silent(
        "<?php\nfunction f(): mixed {\n    $n = 'dyn';\n    $$n = 'ok';\n    return $dyn;\n}\n",
    );
}

#[test]
fn a_variable_variable_read_dams_the_scope() {
    silent("<?php\nfunction f(string $n): mixed {\n    echo $$n;\n    return $whatever;\n}\n");
}

#[test]
fn a_braced_indirect_variable_dams_the_scope() {
    silent(
        "<?php\nfunction f(string $n): mixed {\n    echo ${$n};\n    return $whatever;\n}\n",
    );
}

#[test]
fn eval_dams_the_scope() {
    silent("<?php\nfunction f(string $c): mixed {\n    eval($c);\n    return $minted;\n}\n");
}

#[test]
fn include_dams_the_scope() {
    // An `include` splices names into the including scope, so the closed world
    // ends here — whether or not the path resolves in-universe.
    silent("<?php\nfunction f(): mixed {\n    include 'partial.php';\n    return $fromPartial;\n}\n");
    silent("<?php\nfunction f(): mixed {\n    require __DIR__ . '/p.php';\n    return $fromPartial;\n}\n");
}

// ---------------------------------------------------------------------------
// Static and dynamic property spellings: which `$name` token is a local?
//
// `Class::$prop`'s `$prop` names a slot on the class, not a variable in the frame.
// Missing that made the id fire on one of the commonest shapes in legacy PHP; the
// guzzle fixture below is the exact site that caught it.
// ---------------------------------------------------------------------------

#[test]
fn a_static_property_fetch_is_not_a_variable_read() {
    // Witnessed silent at 8.5.9. The guzzle shape, verbatim:
    // `$client->get(Server::$url)` in tests/ClientTest.php.
    silent("<?php\nfunction f(object $c): void {\n    $c->get(Server::$url);\n}\n");
    silent("<?php\nfunction f(): string {\n    return Server::$url;\n}\n");
}

#[test]
fn late_static_self_and_parent_property_fetches_are_not_variable_reads() {
    for class in ["static", "self", "parent"] {
        silent(&format!(
            "<?php\nclass C extends B {{\n    public static function m(): string {{\n        return {class}::$url;\n    }}\n}}\n"
        ));
    }
}

#[test]
fn a_static_property_write_binds_nothing_and_reads_nothing() {
    // The class carries the state, so the assignment neither creates a local nor
    // reads one (witnessed: `Server::$url = 'set';` is silent).
    silent("<?php\nfunction f(): void {\n    Server::$url = 'set';\n}\n");
}

#[test]
fn a_static_property_fetch_still_reads_a_local_class_expression() {
    // `$obj::$url` — the PROPERTY token is not a local, but the class token is.
    fires("<?php\nfunction f(): string {\n    return $obj::$url;\n}\n", "obj");
}

#[test]
fn a_dynamic_static_property_name_reads_its_local() {
    // Witnessed at 8.5.9: `Server::$$nope` warns `Undefined variable $nope` before
    // it fatals on the empty property name. Not a dam — the indirection reaches the
    // class's static table, where no LOCAL binding can be minted.
    fires("<?php\nfunction f(): string {\n    return Server::$$n;\n}\n", "n");
    fires("<?php\nfunction f(): string {\n    return Server::${$n};\n}\n", "n");
}

#[test]
fn a_bound_dynamic_static_property_name_is_silent() {
    silent("<?php\nfunction f(): string {\n    $n = 'url';\n    return Server::$$n;\n}\n");
}

#[test]
fn a_dynamic_instance_property_name_reads_its_local() {
    // Witnessed at 8.5.9: `$o->$nope2` warns `Undefined variable $nope2`.
    fires("<?php\nfunction f(object $o): mixed {\n    return $o->$n;\n}\n", "n");
}

#[test]
fn a_plain_instance_property_name_is_not_a_variable() {
    silent("<?php\nfunction f(object $o): mixed {\n    return $o->inst;\n}\n");
    silent("<?php\nfunction f(object $o): mixed {\n    return $o?->inst;\n}\n");
}

#[test]
fn a_static_method_called_through_a_local_reads_that_local() {
    // `Server::$m()` is a METHOD call named by `$m` — the CST tells it apart from
    // the property fetch, and PHP warns on the local (witnessed).
    fires("<?php\nfunction f(): mixed {\n    return Server::$m();\n}\n", "m");
}

#[test]
fn a_class_constant_fetch_is_not_a_variable_read() {
    silent("<?php\nfunction f(): mixed {\n    return Server::K;\n}\n");
}

// ---------------------------------------------------------------------------
// Names the engine always binds.
// ---------------------------------------------------------------------------

#[test]
fn superglobals_never_report() {
    for name in
        ["_SERVER", "_GET", "_POST", "_FILES", "_COOKIE", "_SESSION", "_REQUEST", "_ENV"]
    {
        silent(&format!("<?php\nfunction f(): mixed {{\n    return ${name}['k'];\n}}\n"));
    }
}

#[test]
fn globals_never_reports() {
    silent("<?php\nfunction f(): mixed {\n    return $GLOBALS['top'];\n}\n");
}

#[test]
fn this_never_reports() {
    silent("<?php\nclass C {\n    public function m(): mixed {\n        return $this->p;\n    }\n}\n");
}

#[test]
fn http_response_header_never_reports() {
    // Minted by the HTTP stream wrappers into whatever scope made the request,
    // with nothing in the scope's own text to show for it.
    silent(
        "<?php\nfunction f(string $u): array {\n    file_get_contents($u);\n    return $http_response_header;\n}\n",
    );
}

// ---------------------------------------------------------------------------
// The guard trio (ADR-0078 §3): PHP legalizes these reads, so they are not this
// finding at all. Deferred entirely — no id, pending triage.
// ---------------------------------------------------------------------------

#[test]
fn isset_is_not_a_read() {
    // Witnessed at 8.5.9: `isset($u)` on an unbound name is silent and answers false.
    silent("<?php\nfunction f(): bool {\n    return isset($u);\n}\n");
    silent("<?php\nfunction f(): bool {\n    return isset($u['k']);\n}\n");
}

#[test]
fn empty_is_not_a_read() {
    silent("<?php\nfunction f(): bool {\n    return empty($u);\n}\n");
}

#[test]
fn null_coalesce_is_not_a_read_on_its_left() {
    silent("<?php\nfunction f(): string {\n    return $u ?? 'dflt';\n}\n");
}

#[test]
fn null_coalesce_still_judges_its_right() {
    fires("<?php\nfunction f(mixed $u): mixed {\n    return $u ?? $fallback;\n}\n", "fallback");
}

#[test]
fn unset_is_neither_a_read_nor_a_binding() {
    // Witnessed at 8.5.9: `unset($nope)` on an unbound name is silent…
    silent("<?php\nfunction f(): int {\n    unset($nope);\n    return 1;\n}\n");
    // …and it must not be mistaken for a binding either.
    fires("<?php\nfunction f(): mixed {\n    unset($a);\n    return $a;\n}\n", "a");
}

#[test]
fn a_guard_excludes_its_own_read_but_binds_nothing_for_the_rest_of_the_scope() {
    // ADR-0078 §3 excludes the guard trio **at collection** — a guard is not a read
    // — and stops there: it does not make the guarded name bound. So a scope whose
    // ONLY mention of `$x` besides the guard is a plain read still fires on that
    // read. The code is dead (the guard can only ever be false when nothing in the
    // scope binds `$x`), which is why the finding is defensible; it is nonetheless
    // the shape defensive house styles get closest to, and the one for gate triage
    // to watch.
    fires(
        "<?php\nfunction f(): mixed {\n    if (isset($x)) {\n        return $x;\n    }\n    return null;\n}\n",
        "x",
    );
}

#[test]
fn error_control_is_not_a_read() {
    // Witnessed at 8.5.9: `@$nope2` suppresses the warning entirely. The author
    // silenced it in the source; reporting it would be crying wolf.
    silent("<?php\nfunction f(): mixed {\n    return @$nope2;\n}\n");
}

// ---------------------------------------------------------------------------
// The `variable.maybe-undefined` boundary (issue #199) — everything ordering- or
// path-sensitive is silence here, by construction.
// ---------------------------------------------------------------------------

#[test]
fn a_read_before_its_only_assignment_is_silence() {
    // PHP warns; Steins does not. This is `variable.maybe-undefined`'s territory
    // and needs the reachability foundation (issue #199): proving the read comes
    // FIRST is a claim about paths, not about the scope's text.
    silent("<?php\nfunction f(): int {\n    $y = $x;\n    $x = 1;\n    return $y;\n}\n");
}

#[test]
fn a_binding_on_only_some_paths_is_silence() {
    // The `checkMaybeUndefinedVariables` shape, explicitly out of scope (issue #199).
    silent(
        "<?php\nfunction f(bool $c): int {\n    if ($c) {\n        $x = 1;\n    }\n    return $x;\n}\n",
    );
}

#[test]
fn a_binding_in_a_dead_branch_is_still_a_binding() {
    // Ordering-blindness cuts both ways, and that is the point: the finding never
    // depends on a path claim.
    silent(
        "<?php\nfunction f(): int {\n    if (false) {\n        $x = 1;\n    }\n    return $x;\n}\n",
    );
}

// ---------------------------------------------------------------------------
// Scopes that report nothing at all.
// ---------------------------------------------------------------------------

#[test]
fn an_arrow_function_body_captures_instead_of_reading() {
    // Witnessed at 8.5.9: `$x = 3; fn () => $x + 1` is silent — arrow functions
    // auto-capture every free variable from the enclosing scope.
    silent("<?php\nfunction f(): int {\n    $x = 3;\n    $g = fn () => $x + 1;\n    return $g();\n}\n");
}

#[test]
fn an_arrow_function_body_is_silent_even_on_a_name_nothing_binds() {
    // The capture is derived from the body, so an arrow scope cannot prove a name
    // unbound in its own frame; the enclosing scope's question is issue #199's.
    silent("<?php\nfunction f(): callable {\n    return fn () => $nope;\n}\n");
}

#[test]
fn a_nested_arrow_function_does_not_leak_reads_outward() {
    silent("<?php\nfunction f(): callable {\n    return fn () => $nope;\n}\n");
}

#[test]
fn the_top_level_script_scope_never_reports() {
    // `include` splices the INCLUDING scope's whole symbol table into the included
    // file's top level, so a file's own text can never prove a top-level name
    // unbound — the template-partial idiom.
    silent("<?php\necho $title;\n");
}

#[test]
fn a_function_in_a_file_with_a_top_level_read_still_reports() {
    let d = diags("<?php\necho $title;\nfunction f(): int {\n    return $count;\n}\n");
    assert_eq!(d.len(), 1, "only the function scope judges: {d:#?}");
    assert_eq!(d[0].line, 4, "{d:#?}");
}

#[test]
fn a_nested_closure_mention_does_not_bind_the_outer_scope() {
    // The inner closure has its own frame; `$x = 1` inside it binds nothing outside.
    fires(
        "<?php\nfunction f(): mixed {\n    $c = function (): void {\n        $x = 1;\n    };\n    $c();\n    return $x;\n}\n",
        "x",
    );
}

// ---------------------------------------------------------------------------
// The `warning-handler` gate (ADR-0049 §7) — both postures.
// ---------------------------------------------------------------------------

#[test]
fn warning_handler_null_silences() {
    let tree = SourceTree::parse("<?php\nfunction f(): int {\n    return $count;\n}\n");
    let d: Vec<Diagnostic> = check_full(&tree, "test.php", &mut NoFold, false)
        .into_iter()
        .filter(|d| d.id == VARIABLE_UNDEFINED_ID)
        .collect();
    assert!(d.is_empty(), "\"null\" posture silences the warning-grade finding: {d:#?}");
}

#[test]
fn warning_handler_abort_emits() {
    let tree = SourceTree::parse("<?php\nfunction f(): int {\n    return $count;\n}\n");
    let d: Vec<Diagnostic> = check_full(&tree, "test.php", &mut NoFold, true)
        .into_iter()
        .filter(|d| d.id == VARIABLE_UNDEFINED_ID)
        .collect();
    assert_eq!(d.len(), 1, "the default \"abort\" posture emits: {d:#?}");
}
