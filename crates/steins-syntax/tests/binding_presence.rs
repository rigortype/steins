//! The binding-presence pass (ADR-0081, issue #267): some-paths-only reads
//! behind `variable.maybe-undefined`. Every case is a **pair** — the firing
//! shape and its non-firing neighbour — since the pass's whole risk is on the
//! silent side (zero-FP: over-shield, treat unmodelled constructs as
//! unconditional bindings, dam on `goto`; all cost recall, never a finding).
//!
//! `smoke.rs` pins the *definite* pass on the same scopes; the two firing sets
//! are disjoint by construction, asserted in `disjoint_from_the_definite_leg`.

use steins_syntax::{Scope, ScopeOwner, SourceTree};

/// The lowered scope of `function f`, whose body is `body`. `$c` and `$d` are
/// parameters so a branch condition is never itself an unbound read.
fn scope_of(body: &str) -> Scope {
    let src = format!("<?php\nfunction f($c, $d) {{\n{body}\n}}\n");
    let tree = SourceTree::parse(&src);
    tree.scopes()
        .iter()
        .find(|s| matches!(&s.owner, ScopeOwner::Function(n) if n == "f"))
        .expect("the function scope")
        .clone()
}

/// The names `variable.maybe-undefined` would fire on, in source order.
fn maybe(body: &str) -> Vec<String> {
    scope_of(body).maybe_undefined_reads.iter().map(|r| r.name.clone()).collect()
}

/// The names the *definite* leg fires on — the disjointness witness.
fn definite(body: &str) -> Vec<String> {
    scope_of(body).undefined_reads.iter().map(|r| r.name.clone()).collect()
}

fn none() -> Vec<String> {
    Vec::new()
}

fn one(name: &str) -> Vec<String> {
    vec![name.to_owned()]
}

// The some-paths shape — the id's reason to exist.

#[test]
fn some_paths_bind_and_some_do_not() {
    assert_eq!(maybe("if ($c) { $x = 1; } echo $x;"), one("x"));
    assert_eq!(maybe("if ($c) { $x = 1; } elseif ($d) { $y = 2; } echo $x;"), one("x"));
    // Every path binds: silence.
    assert_eq!(maybe("if ($c) { $x = 1; } else { $x = 2; } echo $x;"), none());
    assert_eq!(maybe("$x = 0; if ($c) { $x = 1; } echo $x;"), none());
    // The read is inside the binding arm, so it is reached only after the bind.
    assert_eq!(maybe("if ($c) { $x = 1; echo $x; }"), none());
}

#[test]
fn an_elseif_chain_joins_every_arm() {
    assert_eq!(
        maybe("if ($c) { $x = 1; } elseif ($d) { $x = 2; } else { $x = 3; } echo $x;"),
        none()
    );
    // No `else`: the no-branch path reaches the read unbound.
    assert_eq!(maybe("if ($c) { $x = 1; } elseif ($d) { $x = 2; } echo $x;"), one("x"));
}

#[test]
fn a_literal_condition_is_not_a_branch() {
    // `if_end`'s rule, shared: a proven-true condition adds no no-branch path.
    assert_eq!(maybe("if (true) { $x = 1; } echo $x;"), none());
    // …and a proven-false arm contributes no path of its own.
    assert_eq!(maybe("$x = 0; if (false) { $y = 1; } echo $x;"), none());
}

// Use before assignment — all paths unbound, yet bound later in the text.

#[test]
fn a_read_before_its_only_assignment_fires_on_the_maybe_leg() {
    // The definite id is ordering-blind by contract; promoting this breaks that (non-goal 1).
    assert_eq!(maybe("$y = $x; $x = 1; return $y;"), one("x"));
    assert_eq!(definite("$y = $x; $x = 1; return $y;"), none());
    // Bound first: silence on both legs.
    assert_eq!(maybe("$x = 1; $y = $x; return $y;"), none());
}

#[test]
fn within_one_statement_the_definite_pass_ordering_blindness_is_kept() {
    // An evaluation order this pass does not model must never manufacture a finding.
    assert_eq!(maybe("$a = ($b = 1) + $b; return $a;"), none());
    assert_eq!(maybe("$x = 1; $x .= 'a'; return $x;"), none());
    assert_eq!(maybe("if ($c) { $x = ''; } $x .= 'a'; return $x;"), none());
}

// Termination subtraction — `provably_terminates()`'s first production consumer.

#[test]
fn a_terminating_arm_drops_out_of_the_join() {
    assert_eq!(maybe("if ($c) { $x = 1; } else { return 0; } echo $x;"), none());
    assert_eq!(maybe("if ($c) { $x = 1; } else { throw new RuntimeException(); } echo $x;"), none());
    assert_eq!(maybe("if ($c) { $x = 1; } else { exit; } echo $x;"), none());
    assert_eq!(maybe("if ($c) { $x = 1; } else { die('no'); } echo $x;"), none());
    // The early-return guard, spelled the other way round.
    assert_eq!(maybe("if (!$c) { return 0; } $x = 1; echo $x;"), none());
    // A non-terminating else is the firing counterpart of every line above.
    assert_eq!(maybe("if ($c) { $x = 1; } else { echo 'no'; } echo $x;"), one("x"));
}

#[test]
fn a_statement_after_a_terminator_is_not_judged() {
    // Unreachable code carries no path, so it carries no claim either.
    assert_eq!(maybe("$x = 1; return $x; echo $y;"), none());
}

// Guard polarity — the defaulting idiom (ADR-0081 §5).

#[test]
fn the_defaulting_idiom_is_silent() {
    // The then-arm binds; the implicit else-arm holds `isset($x)`. Join: bound.
    assert_eq!(maybe("if (!isset($x)) { $x = 1; } echo $x;"), none());
    assert_eq!(maybe("if (empty($x)) { $x = 1; } echo $x;"), none());
    assert_eq!(maybe("if (!isset($x)) { $x = 1; } else { $x = 2; } echo $x;"), none());
}

#[test]
fn a_positive_isset_guard_refines_its_own_arm() {
    assert_eq!(maybe("if (isset($x)) { echo $x; } $x = 1;"), none());
    assert_eq!(maybe("if (!empty($x)) { echo $x; } $x = 1;"), none());
    // The read escapes the guarded arm, so the guard says nothing about it.
    assert_eq!(maybe("if (isset($x)) { echo 1; } echo $x; $x = 1;"), one("x"));
}

#[test]
fn a_conjunction_guards_its_right_operand() {
    // `isset($x) && $x > 1` reaches the second read only when `$x` is bound.
    assert_eq!(maybe("if (isset($x) && $x > 1) { echo 'y'; } $x = 1;"), none());
    // `!isset($x) || $x > 1` reaches the second read on the same premise.
    assert_eq!(maybe("if (!isset($x) || $x > 1) { echo 'y'; } $x = 1;"), none());
}

#[test]
fn a_statement_position_assert_refines_everything_after_it() {
    // `assert()` is Verified evidence (ADR-0052 slice I0); a failed enabled
    // assertion throws, so the next statement is reached only on the true polarity.
    assert_eq!(maybe("if ($c) { $x = 1; } assert(isset($x)); echo $x;"), none());
    assert_eq!(maybe("if ($c) { $x = 1; } assert(isset($x) && $x > 0); echo $x;"), none());
    assert_eq!(maybe("if ($c) { $x = 1; } assert(!empty($x)); echo $x;"), none());
    assert_eq!(maybe("if ($c) { $x = 1; } \\assert(isset($x)); echo $x;"), none());
    // A description argument asserts nothing of its own and does not disturb it.
    assert_eq!(maybe("if ($c) { $x = 1; } assert(isset($x), 'why'); echo $x;"), none());

    // `assert(!isset($x))` proves the name ABSENT; no polarity here refines that way.
    assert_eq!(maybe("if ($c) { $x = 1; } assert(!isset($x)); echo $x;"), one("x"));
    assert_eq!(maybe("if ($c) { $x = 1; } assert(empty($x)); echo $x;"), one("x"));
    // A different name, and an assertion about something else entirely.
    assert_eq!(maybe("if ($c) { $x = 1; } assert(isset($d)); echo $x;"), one("x"));
    assert_eq!(maybe("if ($c) { $x = 1; } assert($c); echo $x;"), one("x"));
    // A method named `assert` is not the construct.
    assert_eq!(maybe("if ($c) { $x = 1; } $c->assert(isset($x)); echo $x;"), one("x"));
}

#[test]
fn a_guard_over_an_offset_chain_refines_its_root() {
    // `isset($info[..][..])` cannot be true unless `$info` is bound, so later reads are bound.
    assert_eq!(
        maybe(
            "if ($c) { $info = [1]; } if (!isset($info['a']['b'])) { return 0; } echo $info['a']['b'];"
        ),
        none()
    );
    assert_eq!(maybe("if ($c) { $o = 1; } if (!isset($o->p)) { return 0; } echo $o->p;"), none());
    assert_eq!(maybe("if ($c) { $x = [1]; } if (empty($x['k'])) { return 0; } echo $x['k'];"), none());
    // The negative control: no guard, and the same read reports.
    assert_eq!(maybe("if ($c) { $info = [1]; } echo $info['a'];"), one("info"));
}

#[test]
fn no_polarity_ever_refines_toward_absence() {
    // `isset($x)` is FALSE on a bound null; only the then-arm binding makes the join bound.
    assert_eq!(maybe("if (isset($x)) { $x = 1; } else { $x = 2; } echo $x;"), none());
}

// Loops — the fixpoint (ADR-0081 §4).

#[test]
fn a_binding_inside_a_loop_reaches_the_exit_as_maybe() {
    assert_eq!(maybe("foreach ([1, 2] as $v) { $x = $v; } echo $x;"), one("x"));
    assert_eq!(maybe("while ($c) { $x = 1; } echo $x;"), one("x"));
    assert_eq!(maybe("for ($i = 0; $i < 3; $i++) { $x = 1; } echo $x;"), one("x"));
    // The loop variable itself: zero iterations leaves it unbound.
    assert_eq!(maybe("foreach ([1, 2] as $v) { echo 1; } echo $v;"), one("v"));
    // Bound before the loop: silence.
    assert_eq!(maybe("$x = 0; while ($c) { $x = 1; } echo $x;"), none());
}

#[test]
fn a_do_while_body_runs_at_least_once() {
    assert_eq!(maybe("do { $x = 1; } while ($c); echo $x;"), none());
    // …and a `while (true)` has no false-condition exit edge.
    assert_eq!(maybe("while (true) { $x = 1; if ($c) { break; } } echo $x;"), none());
    assert_eq!(maybe("for (;;) { $x = 1; if ($c) { break; } } echo $x;"), none());
}

#[test]
fn the_back_edge_makes_an_earlier_read_maybe_rather_than_unbound() {
    // A prior iteration may have bound `$x`, so the read is `Maybe`, never the definite id.
    assert_eq!(maybe("foreach ([1, 2] as $v) { echo $x; $x = 1; }"), one("x"));
    assert_eq!(definite("foreach ([1, 2] as $v) { echo $x; $x = 1; }"), none());
    // Bound before the loop: the back edge has nothing to add.
    assert_eq!(maybe("$x = 0; foreach ([1, 2] as $v) { echo $x; $x = 1; }"), none());
}

#[test]
fn a_jumping_arm_does_not_reach_the_ifs_successor() {
    // The corpus's most common shape: the classify-or-skip loop. The `continue`
    // arm never reaches `use($p)`, so the join is over the two binding arms alone.
    assert_eq!(
        maybe(
            "foreach ([1, 2] as $op) { if ($c) { $p = 1; } elseif ($d) { $p = 2; } else { continue; } echo $p; }"
        ),
        none()
    );
    assert_eq!(
        maybe("foreach ([1, 2] as $op) { if ($c) { $p = 1; } else { break; } echo $p; }"),
        none()
    );
    // Negative control: the third-arm-falls-through shape reaches the read, binding nothing.
    assert_eq!(
        maybe(
            "foreach ([1, 2] as $op) { if ($c) { $p = 1; } elseif ($d) { $p = 2; } else { echo 'skip'; } echo $p; }"
        ),
        one("p")
    );
}

#[test]
fn a_break_state_reaches_the_loop_successor() {
    // The only way out of `while (true)` is `break`, and it binds, so the read after is bound.
    assert_eq!(maybe("while (true) { if ($c) { $x = 1; break; } } echo $x;"), none());
    assert_eq!(maybe("for (;;) { if ($c) { $x = 1; break; } } echo $x;"), none());
    // A second break that does NOT bind puts an unbound path back on the exit.
    assert_eq!(
        maybe("while (true) { if ($c) { $x = 1; break; } if ($d) { break; } } echo $x;"),
        one("x")
    );
}

#[test]
fn a_continue_reaches_the_back_edge_rather_than_the_successor() {
    // The `continue` state re-enters the body, so a read before the binding is
    // `Maybe` on the second iteration; the loop's exit still sees zero iterations.
    assert_eq!(maybe("while ($c) { if ($d) { continue; } $x = 1; } echo $x;"), one("x"));
    // Bound before the loop, so neither edge has anything to add.
    assert_eq!(maybe("$x = 0; while ($c) { if ($d) { continue; } $x = 1; } echo $x;"), none());
}

// `try`/`catch`/`finally` — conservative in one direction only (ADR-0081 §4).

#[test]
fn a_catch_arm_enters_with_the_try_block_weakened() {
    // The block may throw before `$x = f()`, so the read after an empty catch is unbound.
    assert_eq!(maybe("try { $x = g(); } catch (Throwable $e) { } echo $x;"), one("x"));
    // The catch binds too, so every path does; weakening normal completion too misreports.
    assert_eq!(maybe("try { $x = g(); } catch (Throwable $e) { $x = 0; } echo $x;"), none());
    // A terminating catch drops out of the join like any other arm.
    assert_eq!(maybe("try { $x = g(); } catch (Throwable $e) { return 0; } echo $x;"), none());
    // A read inside the catch is judged against the weakened state.
    assert_eq!(maybe("try { $x = g(); } catch (Throwable $e) { echo $x; }"), one("x"));
}

#[test]
fn a_try_block_prologue_that_cannot_throw_runs_for_certain() {
    // `$count = 0;` heads the `try` and cannot fail, so every path after carries the binding.
    assert_eq!(
        maybe("try { $count = 0; g(); } catch (Throwable $e) { } echo $count;"),
        none()
    );
    assert_eq!(maybe("try { $out = []; g(); } catch (Throwable $e) { } echo $out;"), none());
    // Negative control: a binding that can throw on its RHS is "may have thrown before this".
    assert_eq!(maybe("try { $count = g(); } catch (Throwable $e) { } echo $count;"), one("count"));
    // …and so is one that follows a statement that can throw.
    assert_eq!(
        maybe("try { g(); $count = 0; } catch (Throwable $e) { } echo $count;"),
        one("count")
    );
}

#[test]
fn a_finally_binding_applies_unconditionally() {
    assert_eq!(
        maybe("try { $x = g(); } catch (Throwable $e) { } finally { $x = 0; } echo $x;"),
        none()
    );
    // The caught variable is bound in its own arm.
    assert_eq!(maybe("try { g(); } catch (Throwable $e) { echo $e; } $e = 1;"), none());
}

// `switch` — the arm that ends in `break` still reaches the successor.

#[test]
fn a_switch_arm_ending_in_break_stays_in_the_join() {
    assert_eq!(
        maybe("switch ($c) { case 1: $x = 1; break; default: $x = 2; break; } echo $x;"),
        none()
    );
    // No `default`: the implicit no-match arm reaches the read unbound.
    assert_eq!(maybe("switch ($c) { case 1: $x = 1; break; } echo $x;"), one("x"));
    // A terminating arm drops out.
    assert_eq!(
        maybe("switch ($c) { case 1: $x = 1; break; default: return 0; } echo $x;"),
        none()
    );
}

#[test]
fn a_switch_case_is_entered_directly_rather_than_fallen_into() {
    // Reaching `case 2` on a match of `2` never ran `case 1`'s binding.
    assert_eq!(maybe("switch ($c) { case 1: $x = 1; case 2: echo $x; }"), one("x"));
}

// Unmodelled constructs read as unconditional bindings — the silent side.

#[test]
fn an_expression_level_branch_is_read_as_an_unconditional_binding() {
    // `?:` and `match` are judged as leaf units here, so a binding inside an arm
    // is unconditional — costs recall, never manufactures a finding (only that direction).
    assert_eq!(maybe("$c ? $x = 1 : null; echo $x;"), none());
    assert_eq!(maybe("match ($c) { 1 => $x = 1, default => null }; echo $x;"), none());
}

#[test]
fn a_goto_dams_the_scope() {
    // A jump to an arbitrary label is an exit edge the traversal cannot bound.
    assert_eq!(maybe("if ($c) { goto done; } $x = 1; done: echo $x;"), none());
}

// Every premise inherited from the definite pass.

#[test]
fn disjoint_from_the_definite_leg() {
    // A name bound nowhere is the definite id's, whatever the paths look like.
    assert_eq!(definite("if ($c) { echo 1; } echo $z;"), one("z"));
    assert_eq!(maybe("if ($c) { echo 1; } echo $z;"), none());
}

#[test]
fn a_name_dam_blanks_both_legs() {
    for dam in [
        "extract($a);",
        "$n = 'x'; $$n = 1;",
        "eval('$x = 1;');",
        "include 'a.php';",
        "get_defined_vars();",
        "compact('x');",
    ] {
        let body = format!("$a = []; if ($c) {{ $x = 1; }} {dam} echo $x;");
        assert_eq!(maybe(&body), none(), "the dam `{dam}` must blank the maybe leg");
        assert_eq!(definite(&body), none(), "the dam `{dam}` must blank the definite leg");
    }
}

#[test]
fn the_guard_exclusions_are_inherited() {
    // PHP legalizes each of these reads, so none of them is this finding at all.
    assert_eq!(maybe("if ($c) { $x = 1; } echo isset($x) ? 1 : 0;"), none());
    assert_eq!(maybe("if ($c) { $x = 1; } echo empty($x) ? 1 : 0;"), none());
    assert_eq!(maybe("if ($c) { $x = 1; } echo $x ?? 'd';"), none());
    assert_eq!(maybe("if ($c) { $x = 1; } unset($x);"), none());
    assert_eq!(maybe("if ($c) { $x = 1; } echo @$x;"), none());
}

#[test]
fn the_engine_bound_names_are_inherited() {
    assert_eq!(maybe("if ($c) { $_GET = []; } echo $_GET;"), none());
    assert_eq!(maybe("if ($c) { $GLOBALS = []; } echo $GLOBALS;"), none());
}

#[test]
fn a_top_level_scope_and_an_arrow_body_report_nothing() {
    let tree = SourceTree::parse("<?php\nif ($c) { $x = 1; }\necho $x;\n");
    for scope in tree.scopes() {
        assert!(scope.maybe_undefined_reads.is_empty(), "a top-level scope never reports");
    }
    let tree = SourceTree::parse("<?php\nfunction f($c) { return fn () => $x; }\n");
    for scope in tree.scopes() {
        assert!(scope.maybe_undefined_reads.is_empty(), "an arrow body never reports");
    }
}

#[test]
fn the_binding_forms_are_inherited_verbatim() {
    // Each binding form the definite pass recognizes must bind on this leg too.
    for bind in [
        "global $x;",
        "static $x;",
        "$x = 1;",
        "$x['k'] = 1;",
        "[$x] = [1];",
        "list($x) = [1];",
        "$x = &$y;",
        "$y = 0; $x = &$y;",
        "++$x;",
        "$x++;",
        "(new DateTime())->format($x);",
        "DateTime::createFromFormat($x, $d);",
        "$c->m($x);",
    ] {
        let body = format!("{bind} echo $x;");
        assert_eq!(maybe(&body), none(), "`{bind}` must bind on the maybe leg");
    }
}

#[test]
fn a_closure_use_clause_speaks_about_this_scope() {
    // A by-value `use ($x)` READS the enclosing binding…
    assert_eq!(maybe("if ($c) { $x = 1; } $f = function () use ($x) { return $x; };"), one("x"));
    // …while a by-ref `use (&$x)` creates it.
    assert_eq!(maybe("$f = function () use (&$x) { return $x; }; echo $x;"), none());
    // A closure's own body is its own scope's question.
    assert_eq!(maybe("$f = function ($c) { if ($c) { $y = 1; } return $y; }; return $f;"), none());
}

#[test]
fn a_closure_body_is_judged_as_its_own_scope() {
    let src = "<?php\nfunction f($c) { return function () use ($c) { if ($c) { $y = 1; } return $y; }; }\n";
    let tree = SourceTree::parse(src);
    let names: Vec<String> = tree
        .scopes()
        .iter()
        .filter(|s| matches!(s.owner, ScopeOwner::Closure { .. }))
        .flat_map(|s| s.maybe_undefined_reads.iter().map(|r| r.name.clone()))
        .collect();
    assert_eq!(names, one("y"));
}

#[test]
fn a_function_call_argument_is_left_for_the_checker() {
    // `preg_match($p, $s, $m)` BINDS `$m`; whether an arg is by-reference is the
    // callee's property, so the read is recorded here, subtracted by the checker (ADR-0077).
    let scope = scope_of("if ($c) { $m = []; } preg_match('/a/', 'b', $m); echo $m;");
    assert!(
        scope.ref_arg_candidates.iter().any(|r| r.name == "m"),
        "the out-parameter candidate must survive onto the scope"
    );
}
