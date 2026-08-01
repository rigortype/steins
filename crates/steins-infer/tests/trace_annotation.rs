//! ADR-0074 issue #94 — the trace annotation's tracer bullet: a statement-adopted
//! `/** @psalm-trace $x */` docblock emits `debug.trace` carrying the engine's
//! best fact for `$x`, end to end.
//!
//! The annotation is a second SPELLING of the question `PHPStan\dumpType($x)`
//! asks (§5): same fact source (the trust-ordered lookup), same renderer (the
//! one honesty renderer, `(asserted)` marker included), same descent gating
//! (a site emits once). Only the trigger and the message label differ — and the
//! position semantics are Psalm's: the answer is the adopted statement's
//! **EXIT facts**, what `dumpType($x)` would report were it the *following*
//! statement. The matrix here covers:
//!
//! * recognition (§2): `@psalm-trace` canonical (Psalm's own doc example is the
//!   headline test), `@phpstan-trace` via the uniform strip, bare `@trace` not
//!   a tag;
//! * answer semantics (§5): the post-assignment state, guard-narrowed facts,
//!   diverging statements (`return $x;` still answers), the `(asserted)`
//!   stratum marker, honest `unknown`, byte-parity with the dump surface and
//!   with the `annotate` margin;
//! * placement (§6, the SHARED statement-adoption rule — `stmt_docblock`,
//!   identical for the inline `@var` cast): a blank line does not break
//!   adoption, a trailing docblock adopts forward, an intervening line comment
//!   silences; declaration docblocks (function/class/interface/enum/trait) are
//!   inert at the emitter;
//! * emission discipline (§5/§9): once per site (never per caller), the last
//!   statement of a scope included, reported at the TAG's own position, and
//!   transparent — the non-debug diagnostic set of a file is identical with
//!   and without the annotation (the ADR-0053 §10 dump-transparency fixture,
//!   transposed).
//!
//! The rendered fact after the final ": " is the pinned part of the message;
//! the frame wording (`traced type of $x: …`) is not a contract (ADR-0023).

use steins_infer::{
    DEBUG_TRACE_ID, DEBUG_TYPE_ID, Diagnostic, FactKind, NoFold, annotate_facts, check,
};
use steins_syntax::SourceTree;

/// The `debug.trace` diagnostics a source file produces.
fn traces(src: &str) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check(&tree, &[], "t.php").into_iter().filter(|d| d.id == DEBUG_TRACE_ID).collect()
}

/// The single `debug.trace` diagnostic a one-annotation source produces.
fn one_trace(src: &str) -> Diagnostic {
    let ts = traces(src);
    assert_eq!(ts.len(), 1, "expected exactly one debug.trace report, got {ts:?}");
    ts.into_iter().next().expect("length checked above")
}

// ---- Recognition + the canonical example (ADR-0074 §2/§5) -------------------

#[test]
fn the_canonical_psalm_example_reports_the_post_statement_type() {
    // Psalm's own documentation example — `/** @psalm-trace $username */`
    // above `$username = $_GET['username'];` prints what `$username` BECAME —
    // pins the exit-facts semantics (§5): the annotation is "applied to the
    // next statement" and reports the type that statement leaves behind. The
    // compat spelling carries the compat semantics.
    let src = "<?php\n/** @psalm-trace $x */\n$x = 5;\n";
    assert_eq!(one_trace(src).message, "traced type of $x: 5");
}

#[test]
fn a_statement_not_touching_the_variable_reports_its_standing_fact() {
    let src = "<?php\n$x = 5;\n/** @psalm-trace $x */\n$y = $x;\n";
    assert_eq!(one_trace(src).message, "traced type of $x: 5");
}

#[test]
fn phpstan_trace_rides_the_uniform_prefix_strip() {
    let src = "<?php\n/** @phpstan-trace $x */\n$x = 'GET';\n";
    assert_eq!(one_trace(src).message, "traced type of $x: 'GET'");
}

#[test]
fn bare_trace_is_not_a_tag() {
    // Neither upstream tool recognizes an unprefixed `@trace` (§2, the
    // assertion-family precedent) — silence, and the docblock stays an
    // ordinary comment.
    let src = "<?php\n/** @trace $x */\n$x = 5;\n";
    assert!(traces(src).is_empty());
}

// ---- Answer semantics (ADR-0074 §5) ----------------------------------------

#[test]
fn a_guard_narrowed_fact_is_the_answer() {
    // Position semantics are the walk's, unchanged: inside an `is_int` branch
    // the annotated statement's exit facts carry the narrowing, exactly as a
    // `dumpType` after it would.
    let src = "<?php\n/** @param int|string $x */\nfunction f($x): void {\n\
               if (is_int($x)) {\n/** @psalm-trace $x */\n$y = 1;\n}\n}\n";
    assert_eq!(one_trace(src).message, "traced type of $x: int");
}

#[test]
fn a_return_under_the_annotation_still_answers() {
    // A diverging adopted statement still answers (§5): there is no "next
    // statement", but the question was asked and the exit state exists.
    let src = "<?php\nfunction f(int $v): int {\n/** @psalm-trace $v */\nreturn $v;\n}\n";
    assert_eq!(one_trace(src).message, "traced type of $v: int");
}

#[test]
fn an_asserted_premise_carries_the_marker() {
    // A docblock-asserted narrowing (the `@phpstan-assert` tag family) is an
    // Asserted-stratum premise — the trace carries the `(asserted)` marker so
    // the introspection surface never launders a claim into a proven value
    // (ADR-0052 §5, shared with the dump path).
    let src = "<?php\n/** @phpstan-assert null $x */\nfunction claimNull($x): void {}\n\
               function f($x): void {\nclaimNull($x);\n/** @psalm-trace $x */\n$y = 1;\n}\n";
    assert_eq!(one_trace(src).message, "traced type of $x: null (asserted)");
}

#[test]
fn a_never_assigned_variable_reports_honest_unknown() {
    // A name with no fact at the statement's exit renders `unknown` like any
    // other unanswerable dump — honest incompleteness, never silence, never a
    // guess.
    let src = "<?php\n/** @psalm-trace $nope */\n$x = 1;\n";
    assert_eq!(one_trace(src).message, "traced type of $nope: unknown");
}

#[test]
fn the_rendered_fact_is_byte_equal_with_the_dump_surface() {
    // §5's governing principle, mechanized: the trace above statement S is the
    // question `dumpType($x)` asks as the FOLLOWING statement — so the two
    // rendered facts must agree byte-for-byte.
    let src = "<?php\n/** @psalm-trace $x */\n$x = 5;\n\\PHPStan\\dumpType($x);\n";
    let tree = SourceTree::parse(src);
    let all = check(&tree, &[], "t.php");
    let trace_fact = all
        .iter()
        .find(|d| d.id == DEBUG_TRACE_ID)
        .and_then(|d| d.message.strip_prefix("traced type of $x: "))
        .expect("one trace report with the expected frame");
    let dump_fact = all
        .iter()
        .find(|d| d.id == DEBUG_TYPE_ID)
        .and_then(|d| d.message.strip_prefix("dumped type: "))
        .expect("one dump report with the expected frame");
    assert_eq!(trace_fact, dump_fact, "one question, one answer machinery");
}

#[test]
fn the_rendered_fact_is_byte_equal_with_the_annotate_margin() {
    // The annotate byte-parity obligation extends to this id (§5): the margin
    // records the assignment's post-statement fact at the assignment's line —
    // exactly the position the trace above it answers for — and the two
    // spellings are byte-equal. (An int literal — the margin and the dump path
    // share its spelling; the string-literal quoting divergence between the
    // two surfaces predates this id and is not reopened here.)
    let src = "<?php\n/** @psalm-trace $x */\n$x = 5;\n$y = $x;\n";
    let trace_fact = one_trace(src)
        .message
        .strip_prefix("traced type of $x: ")
        .expect("the expected frame")
        .to_owned();
    let tree = SourceTree::parse(src);
    let margin = annotate_facts(&tree, &[], &[], "t.php", &mut NoFold)
        .into_iter()
        .find_map(|f| match f.kind {
            FactKind::Value { ref var, ref rendered } if var == "x" => Some(rendered.clone()),
            _ => None,
        })
        .expect("annotate renders a value fact for $x");
    assert_eq!(trace_fact, margin, "the margin and the trace spell the same fact");
}

// ---- Placement (ADR-0074 §6: the shared statement-adoption rule) -----------

#[test]
fn a_blank_line_detached_docblock_still_adopts() {
    // The shared rule (`stmt_docblock`, identical for `@var` casts) does not
    // break adoption on a blank line: the nearest preceding trivium is the
    // docblock and the gap is all whitespace, so the annotation still answers.
    // (An earlier draft's one-line-break maximum was dropped when the shared
    // rule landed first — §6.)
    let src = "<?php\n/** @psalm-trace $x */\n\n$x = 1;\n";
    assert_eq!(one_trace(src).message, "traced type of $x: 1");
}

#[test]
fn a_trailing_docblock_adopts_forward_onto_the_next_statement() {
    // A docblock trailing another statement's line adopts FORWARD onto the
    // next statement under the shared rule (§6) — the gap to the next
    // statement's start is whitespace-only.
    let src = "<?php\n$a = 1; /** @psalm-trace $a */\n$b = 2;\n";
    assert_eq!(one_trace(src).message, "traced type of $a: 1");
}

#[test]
fn an_intervening_line_comment_is_silent() {
    // Any intervening non-whitespace — line comments included — breaks
    // adoption (the nearest preceding trivium is then not a docblock); the tag
    // triggers nothing, silently.
    let src = "<?php\n/** @psalm-trace $x */\n// note\n$x = 1;\n";
    assert!(traces(src).is_empty());
}

#[test]
fn declaration_docblocks_are_inert() {
    // Declaration statements are inert AT THE EMITTER (§6): the shared
    // adoption query is tag-agnostic and returns declaration-leading docblocks
    // too, so the trace-specific exclusion — all five declaration kinds —
    // lives with the emitter's own guard (`stmt_is_declaration`).
    let function = "<?php\n$x = 1;\n/** @psalm-trace $x */\nfunction g(): void {}\n";
    assert!(traces(function).is_empty(), "function docblock is a contract surface");
    let class = "<?php\n$x = 1;\n/** @psalm-trace $x */\nclass C {}\n";
    assert!(traces(class).is_empty(), "class docblock is a contract surface");
    let interface = "<?php\n$x = 1;\n/** @psalm-trace $x */\ninterface I {}\n";
    assert!(traces(interface).is_empty(), "interface docblock is a contract surface");
    let en = "<?php\n$x = 1;\n/** @psalm-trace $x */\nenum E {}\n";
    assert!(traces(en).is_empty(), "enum docblock is inert at the emitter");
    let tr = "<?php\n$x = 1;\n/** @psalm-trace $x */\ntrait T {}\n";
    assert!(traces(tr).is_empty(), "trait docblock is inert at the emitter");
}

// ---- Emission discipline (ADR-0074 §5/§9) ----------------------------------

#[test]
fn an_annotated_site_in_a_shared_function_emits_once() {
    // Descent-gated exactly like the dump calls: the emitter runs in the plain
    // per-scope pass only, so an annotated statement inside a function called
    // from two sites reports once — never per caller.
    let src = "<?php\nfunction f(int $v): int {\n/** @psalm-trace $v */\n$y = $v;\n\
               return $y;\n}\nf(1);\nf(2);\n";
    let ts = traces(src);
    assert_eq!(ts.len(), 1, "one annotated site, one report: {ts:?}");
    assert_eq!(ts[0].message, "traced type of $v: int");
}

#[test]
fn the_last_statement_of_a_scope_emits() {
    // The flush at the iteration's common exit covers the final statement of a
    // trace too — and the answer is the post-assignment state (the second
    // assignment's own fact, not the first's).
    let src = "<?php\nfunction f(): void {\n$a = 1;\n/** @psalm-trace $a */\n$a = 2;\n}\n";
    assert_eq!(one_trace(src).message, "traced type of $a: 2");
}

#[test]
fn the_report_sits_at_the_tags_own_position() {
    // The diagnostic position is the TAG's line/column — the question's own
    // text, like a dump call reports at the call (orchestrator-settled).
    let src = "<?php\n$a = 1;\n/** @psalm-trace $a */\n$b = 2;\n";
    let t = one_trace(src);
    assert_eq!(t.line, 3, "the docblock's line, not the statement's");
    // `/** ` is four characters; the `@` sits at 1-based column 5.
    assert_eq!(t.column, 5, "the tag's `@`, not the block's `/**`");
}

#[test]
fn the_annotation_is_transparent_to_every_other_diagnostic() {
    // §9 (the ADR-0053 §10 dump-transparency fixture, transposed): the
    // annotation reads facts and binds nothing, so the full non-debug
    // diagnostic set of a file is identical with and without it. The two
    // sources differ only in line 3 — a plain comment vs the annotation — so
    // every position lines up for the byte comparison.
    let without = "<?php\nfunction width(int $w): int { return $w; }\n// note\n\
                   $x = 5;\nwidth('nope');\n";
    let with = "<?php\nfunction width(int $w): int { return $w; }\n/** @psalm-trace $x */\n\
                $x = 5;\nwidth('nope');\n";
    let non_debug = |src: &str| -> Vec<(&'static str, u32, u32, String)> {
        let tree = SourceTree::parse(src);
        check(&tree, &[], "t.php")
            .into_iter()
            .filter(|d| !d.id.starts_with("debug."))
            .map(|d| (d.id, d.line, d.column, d.message))
            .collect()
    };
    let base = non_debug(without);
    assert!(!base.is_empty(), "the fixture must carry a real (non-debug) finding");
    assert_eq!(non_debug(with), base, "the annotation perturbs no other diagnostic");
    // …and the annotated version answers the asked question with the annotated
    // assignment's own exit fact (§5).
    let ts = traces(with);
    assert_eq!(ts.len(), 1, "the annotation itself reports: {ts:?}");
    assert_eq!(ts[0].message, "traced type of $x: 5");
}
