//! Differential check against the Lean 4 specification of this crate (ADR-0059).
//!
//! `spike/lean-domain` proves the domain's soundness contract and then prints a
//! deterministic vector file (`lake exe vectors`, committed as
//! `fixtures/lean-vectors.expected`). This test walks the *same* universe in the
//! *same* order through the real Rust implementation and compares the rendered
//! results line by line.
//!
//! What a failure means, in order of likelihood:
//!
//! 1. the Rust implementation changed behaviour (the interesting case — the
//!    fixture is what the proofs are about);
//! 2. the universe or a renderer drifted on one side only (fix the mismatch,
//!    then regenerate with `cargo xtask lean-check --bless`);
//! 3. the spec was edited and the fixture regenerated without re-running this
//!    test.
//!
//! Comment lines in the fixture are documentation; the contract is the data
//! lines, which is what this test compares.

use steins_domain::{
    Base, Certainty, Fact, IntRange, Key, Refinement, StrPreds, Val, php_is_falsy,
};

/// The string atoms of the spec, in rank order — which is `str::cmp` order,
/// because the spec models a string value as its position in the total order.
/// The trailing field of each `atom` line is `StrPreds::of` applied to these:
/// the classifier check.
const STR_ATOMS: [&str; 6] = ["", " 5 ", "0", "00", "5", "abc"];

/// The float atoms, with the literal text the spec prints for each.
const FLOAT_ATOMS: [(f64, &str); 3] = [(-1.5, "-1.5"), (0.0, "0.0"), (2.5, "2.5")];

/// The array atoms, with their literal text.
fn arr_atoms() -> Vec<(Val, &'static str)> {
    vec![
        (Val::Array(vec![]), "[]"),
        (Val::Array(vec![(Key::Int(0), Val::Int(1))]), "[0=>1]"),
    ]
}

// ----------------------------------------------------------------------------
// Rendering — must agree with `SteinsDomain.Vectors` byte for byte.
// ----------------------------------------------------------------------------

fn render_int(n: i64) -> String {
    if n == i64::MIN {
        "min".to_owned()
    } else if n == i64::MAX {
        "max".to_owned()
    } else {
        n.to_string()
    }
}

fn render_preds(p: StrPreds) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if p.contains_all(StrPreds::NON_EMPTY) {
        parts.push("NE");
    }
    if p.contains_all(StrPreds::NON_FALSY) {
        parts.push("NF");
    }
    if p.contains_all(StrPreds::NUMERIC) {
        parts.push("NUM");
    }
    if parts.is_empty() { "-".to_owned() } else { parts.join("|") }
}

fn render_range(r: IntRange) -> String {
    format!("[{},{}]", render_int(r.lo()), render_int(r.hi()))
}

fn render_base(b: Base) -> &'static str {
    match b {
        Base::Int => "int",
        Base::Float => "float",
        Base::String => "str",
        Base::Bool => "bool",
    }
}

fn render_refinement(r: &Refinement) -> String {
    match r {
        Refinement::Str(p) => format!("str{{{}}}", render_preds(*p)),
        Refinement::Int(q) => format!("int{}", render_range(*q)),
    }
}

fn float_rank(f: f64) -> usize {
    FLOAT_ATOMS
        .iter()
        .position(|(x, _)| x.to_bits() == f.to_bits())
        .unwrap_or_else(|| panic!("float {f} is not an atom of the vector universe"))
}

fn str_rank(s: &str) -> usize {
    STR_ATOMS
        .iter()
        .position(|a| *a == s)
        .unwrap_or_else(|| panic!("string {s:?} is not an atom of the vector universe"))
}

fn arr_rank(v: &Val) -> usize {
    arr_atoms()
        .iter()
        .position(|(a, _)| a == v)
        .unwrap_or_else(|| panic!("array {v:?} is not an atom of the vector universe"))
}

fn render_val(v: &Val) -> String {
    match v {
        Val::Null => "null".to_owned(),
        Val::Bool(b) => if *b { "true" } else { "false" }.to_owned(),
        Val::Int(i) => format!("int:{}", render_int(*i)),
        Val::Float(f) => format!("float#{}", float_rank(*f)),
        Val::Str(s) => format!("str#{}", str_rank(s)),
        Val::Array(_) => format!("arr#{}", arr_rank(v)),
    }
}

fn render_nullable(n: bool) -> &'static str {
    if n { "null" } else { "nonnull" }
}

fn render_fact(f: &Fact) -> String {
    match f {
        Fact::Singleton(v) => format!("S({})", render_val(v)),
        Fact::OneOf(vs) => {
            let rendered: Vec<String> = vs.iter().map(render_val).collect();
            format!("O({})", rendered.join(","))
        }
        Fact::Refined { base, refinement, nullable } => format!(
            "R({},{},{})",
            render_base(*base),
            render_refinement(refinement),
            render_nullable(*nullable)
        ),
        Fact::General { base, nullable } => {
            format!("G({},{})", render_base(*base), render_nullable(*nullable))
        }
    }
}

fn render_opt_fact(f: Option<&Fact>) -> String {
    match f {
        None => "TOP".to_owned(),
        Some(g) => render_fact(g),
    }
}

fn render_cert(c: Certainty) -> &'static str {
    match c {
        Certainty::Yes => "yes",
        Certainty::No => "no",
        Certainty::Maybe => "maybe",
    }
}

// ----------------------------------------------------------------------------
// The universe — same construction and same order as `SteinsDomain.Vectors`.
// ----------------------------------------------------------------------------

fn values() -> Vec<Val> {
    let mut out = vec![Val::Null, Val::Bool(false), Val::Bool(true)];
    for i in [i64::MIN, -1, 0, 1, 2, 8, 9, i64::MAX] {
        out.push(Val::Int(i));
    }
    for (f, _) in FLOAT_ATOMS {
        out.push(Val::Float(f));
    }
    for s in STR_ATOMS {
        out.push(Val::Str(s.to_owned()));
    }
    for (a, _) in arr_atoms() {
        out.push(a);
    }
    out
}

/// The seven predicate sets a caller can build through `StrPreds`. The eighth
/// subset — `{NonFalsy, Numeric}` without `NonEmpty` — is unreachable, because
/// `union` closes and `intersect` cannot add bits.
fn preds_universe() -> Vec<StrPreds> {
    vec![
        StrPreds::empty(),
        StrPreds::NON_EMPTY,
        StrPreds::NON_FALSY,
        StrPreds::NUMERIC,
        StrPreds::NON_FALSY.close(),
        StrPreds::NUMERIC.close(),
        StrPreds::NON_FALSY.union(StrPreds::NUMERIC),
    ]
}

fn range_universe() -> Vec<IntRange> {
    vec![
        IntRange::FULL,
        IntRange::POSITIVE,
        IntRange::NEGATIVE,
        IntRange::NON_NEGATIVE,
        IntRange::point(0),
        IntRange::new(1, 9).expect("ordered"),
        IntRange::new(-5, 5).expect("ordered"),
    ]
}

fn one_of_seeds() -> Vec<Vec<Val>> {
    vec![
        vec![Val::Int(1), Val::Int(2)],
        // exactly CAP members: the last set that stays in the finite layer
        vec![
            Val::Int(i64::MIN),
            Val::Int(-1),
            Val::Int(0),
            Val::Int(1),
            Val::Int(2),
            Val::Int(8),
            Val::Int(9),
            Val::Int(i64::MAX),
        ],
        vec![Val::Str("0".to_owned()), Val::Str("5".to_owned())],
        vec![Val::Null, Val::Int(1)],
        vec![Val::Bool(false), Val::Bool(true)],
        vec![Val::Int(1), Val::Str("5".to_owned())],
        vec![Val::Float(-1.5), Val::Float(0.0)],
    ]
}

fn facts() -> Vec<Fact> {
    let mut out: Vec<Fact> = Vec::new();
    for v in [
        Val::Null,
        Val::Bool(false),
        Val::Int(0),
        Val::Int(1),
        Val::Int(9),
        Val::Str(String::new()),
        Val::Str("5".to_owned()),
        Val::Float(0.0),
        Val::Array(vec![]),
    ] {
        out.push(Fact::singleton(v));
    }
    for seed in one_of_seeds() {
        if let Some(f) = Fact::from_vals(seed) {
            out.push(f);
        }
    }
    for p in preds_universe() {
        out.push(Fact::refined(Base::String, Refinement::Str(p), false));
        out.push(Fact::refined(Base::String, Refinement::Str(p), true));
    }
    for q in range_universe() {
        out.push(Fact::refined(Base::Int, Refinement::Int(q), false));
        out.push(Fact::refined(Base::Int, Refinement::Int(q), true));
    }
    for base in [Base::Int, Base::Float, Base::String, Base::Bool] {
        out.push(Fact::General { base, nullable: false });
        out.push(Fact::General { base, nullable: true });
    }
    // `List.eraseDups`: keep the first occurrence, preserve order. The
    // normalising `Fact::refined` collapses the empty predicate set and the full
    // interval to General, which is where the duplicates come from.
    let mut deduped: Vec<Fact> = Vec::with_capacity(out.len());
    for f in out {
        if !deduped.contains(&f) {
            deduped.push(f);
        }
    }
    deduped
}

/// `None` is ⊤ and absorbs — the reading `join_envs` implements when it drops a
/// binding.
fn join_opt(a: Option<&Fact>, b: Option<&Fact>) -> Option<Fact> {
    match (a, b) {
        (Some(x), Some(y)) => x.join(y),
        _ => None,
    }
}

// ----------------------------------------------------------------------------
// Generation
// ----------------------------------------------------------------------------

fn generate() -> Vec<String> {
    let vals = values();
    let fs = facts();
    let preds = preds_universe();
    let ranges = range_universe();
    let mut out: Vec<String> = vec!["version 1".to_owned()];

    for (i, s) in STR_ATOMS.iter().enumerate() {
        out.push(format!("atom str#{i} {s:?} {}", render_preds(StrPreds::of(s))));
    }
    for (i, (f, lit)) in FLOAT_ATOMS.iter().enumerate() {
        let falsy = if php_is_falsy(&Val::Float(*f)) { "falsy" } else { "truthy" };
        out.push(format!("atom float#{i} {lit} {falsy}"));
    }
    for (i, (a, lit)) in arr_atoms().iter().enumerate() {
        let falsy = if php_is_falsy(a) { "falsy" } else { "truthy" };
        out.push(format!("atom arr#{i} {lit} {falsy}"));
    }

    let mut ordered = vals.clone();
    ordered.sort();
    ordered.dedup();
    let rendered: Vec<String> = ordered.iter().map(render_val).collect();
    out.push(format!("order {}", rendered.join(" ")));

    for f in &fs {
        for v in &vals {
            out.push(format!(
                "admits {} {} => {}",
                render_fact(f),
                render_val(v),
                f.admits(v)
            ));
        }
    }
    for f in &fs {
        out.push(format!("truthy {} => {}", render_fact(f), render_cert(f.truthy())));
    }
    for f in &fs {
        out.push(format!("isnull {} => {}", render_fact(f), render_cert(f.is_null())));
    }
    for f in &fs {
        for p in &preds {
            out.push(format!(
                "satisfiesstr {} {} => {}",
                render_fact(f),
                render_preds(*p),
                render_cert(f.satisfies_str(*p))
            ));
        }
    }
    for f in &fs {
        for r in &ranges {
            out.push(format!(
                "intin {} {} => {}",
                render_fact(f),
                render_range(*r),
                render_cert(f.int_in(*r))
            ));
        }
    }
    for a in &fs {
        for b in &fs {
            out.push(format!(
                "join {} {} => {}",
                render_fact(a),
                render_fact(b),
                render_opt_fact(a.join(b).as_ref())
            ));
        }
    }

    let mut total = 0usize;
    let mut mismatches = 0usize;
    for a in &fs {
        for b in &fs {
            let ab = a.join(b);
            for c in &fs {
                let bc = b.join(c);
                let lhs = join_opt(ab.as_ref(), Some(c));
                let rhs = join_opt(Some(a), bc.as_ref());
                total += 1;
                if lhs != rhs {
                    mismatches += 1;
                }
            }
        }
    }
    out.push(format!("assoc {total} {mismatches}"));
    out
}

fn expected() -> Vec<String> {
    let raw = include_str!("fixtures/lean-vectors.expected");
    raw.lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

// ----------------------------------------------------------------------------
// The check
// ----------------------------------------------------------------------------

#[test]
fn rust_agrees_with_the_lean_specification() {
    let got = generate();
    let want = expected();

    assert_eq!(want.first().map(String::as_str), Some("version 1"), "fixture version");

    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        assert_eq!(
            g, w,
            "line {i} disagrees with the Lean spec\n  rust: {g}\n  lean: {w}\n\
             (see the header of this test for what a mismatch means)"
        );
    }
    assert_eq!(
        got.len(),
        want.len(),
        "vector count differs: rust produced {}, the spec {} — the universes drifted",
        got.len(),
        want.len()
    );
}

/// The associativity tally is data in the vector file, so a regression would
/// merely change a committed number. Assert the value the spike established
/// separately: `join` is associative over the whole vector universe.
#[test]
fn join_is_associative_over_the_vector_universe() {
    let line = generate()
        .into_iter()
        .find(|l| l.starts_with("assoc "))
        .expect("assoc line");
    let mut parts = line.split_whitespace().skip(1);
    let total: usize = parts.next().expect("total").parse().expect("number");
    let mismatches: usize = parts.next().expect("mismatches").parse().expect("number");
    assert!(total > 100_000, "the universe should cover a six-figure triple count, got {total}");
    assert_eq!(mismatches, 0, "join is not associative over the vector universe");
}
