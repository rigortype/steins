//! Differential check against the Lean 4 spec of this crate (ADR-0059): walks
//! the same universe/order as `spike/lean-domain`'s `lake exe vectors` output
//! (`fixtures/lean-vectors.expected`) through Rust, comparing line by line
//! (data lines are the contract; fixture comments are docs). A failure usually
//! means Rust's behaviour changed, the universe/renderer drifted, or the
//! fixture wasn't regenerated — fix via `cargo xtask lean-check --bless`.

use steins_domain::{
    Base, Certainty, Cover, CoverFlavor, Fact, IntRange, Key, KeyClass, PhpStr, Presence,
    Refinement, ShapeFact, StrPreds, Tail, Val, array_is_list, php_is_falsy,
};

/// String atoms, `str::cmp` rank order (a string's position in the total order);
/// trailing `atom`-line field is `StrPreds::of`. `"ABC"` separates casing (`'A' < 'a'`);
/// `"00"`/`" 5 "` separate `decimal-int-string` from `numeric-string` (non-canonical).
const STR_ATOMS: [&str; 7] = ["", " 5 ", "0", "00", "5", "ABC", "abc"];

/// The float atoms, with the literal text the spec prints for each.
const FLOAT_ATOMS: [(f64, &str); 3] = [(-1.5, "-1.5"), (0.0, "0.0"), (2.5, "2.5")];

/// String-key atoms of the shape section, `str::cmp` rank order (see `STR_ATOMS`).
const KEY_ATOMS: [&str; 3] = ["a", "b", "c"];

/// S4 narrowing-operator keys: the array atoms' own keys, so each op sees a hit and a miss.
fn op_keys() -> Vec<Key> {
    vec![Key::Int(0), Key::Int(1), Key::Str("a".into()), Key::Str("b".into())]
}

/// Every 2-element subset of [`op_keys`]: exercises full, half, and no declaration per seed.
fn cover_pairs() -> Vec<(Key, Key)> {
    let ks = op_keys();
    let mut out = Vec::new();
    for i in 0..ks.len() {
        for j in (i + 1)..ks.len() {
            out.push((ks[i].clone(), ks[j].clone()));
        }
    }
    out
}

fn render_flavor(f: CoverFlavor) -> &'static str {
    match f {
        CoverFlavor::Isset => "i",
        CoverFlavor::KeyExists => "e",
    }
}

fn ik(n: i64) -> Key {
    Key::Int(n)
}

fn sk(rank: usize) -> Key {
    Key::Str(KEY_ATOMS[rank].into())
}

/// Every array atom, in `Val`'s total order (= `Val::arr rank` on the Lean side —
/// ranks are load-bearing). Ranks 0–1 are the pre-shape universe; the rest are shape-only.
fn shape_arr_atoms() -> Vec<(Val, &'static str)> {
    let a = |entries: Vec<(Key, Val)>| Val::Array(entries);
    vec![
        (a(vec![]), "[]"),
        (a(vec![(ik(0), Val::Int(1))]), "[0=>1]"),
        (a(vec![(ik(0), Val::Int(1)), (ik(1), Val::Int(2))]), "[0=>1,1=>2]"),
        (a(vec![(ik(0), Val::Int(1)), (sk(0), Val::Int(2))]), "[0=>1,a=>2]"),
        (a(vec![(ik(1), Val::Int(2))]), "[1=>2]"),
        (a(vec![(sk(0), Val::Int(1))]), "[a=>1]"),
        (a(vec![(sk(0), Val::Int(2))]), "[a=>2]"),
        (a(vec![(sk(0), Val::Int(2)), (sk(1), Val::Int(1))]), "[a=>2,b=>1]"),
        (a(vec![(sk(0), Val::Int(3))]), "[a=>3]"),
        (a(vec![(sk(0), Val::Int(4))]), "[a=>4]"),
        (a(vec![(sk(0), Val::Int(5))]), "[a=>5]"),
        (a(vec![(sk(0), Val::Int(6))]), "[a=>6]"),
        (a(vec![(sk(0), Val::Int(7))]), "[a=>7]"),
        (a(vec![(sk(0), Val::Int(8))]), "[a=>8]"),
        (a(vec![(sk(0), Val::Int(9))]), "[a=>9]"),
        (a(vec![(sk(1), Val::Int(1))]), "[b=>1]"),
    ]
}

/// The array atoms of the pre-shape universe, with their literal text.
fn arr_atoms() -> Vec<(Val, &'static str)> {
    shape_arr_atoms().into_iter().take(2).collect()
}

fn arr_atom(rank: usize) -> Val {
    shape_arr_atoms()[rank].0.clone()
}

fn arr_entries(rank: usize) -> Vec<(Key, Val)> {
    match arr_atom(rank) {
        Val::Array(e) => e,
        other => panic!("atom {rank} is not an array: {other:?}"),
    }
}

// Rendering — must agree with `SteinsDomain.Vectors` byte for byte.

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
    if p.contains_all(StrPreds::LOWERCASE) {
        parts.push("LC");
    }
    if p.contains_all(StrPreds::UPPERCASE) {
        parts.push("UC");
    }
    if p.contains_all(StrPreds::DECIMAL_INT) {
        parts.push("DEC");
    }
    if p.contains_all(StrPreds::NON_DECIMAL_INT) {
        parts.push("NDEC");
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

fn str_rank(s: &PhpStr) -> usize {
    STR_ATOMS
        .iter()
        .position(|a| *s == *a)
        .unwrap_or_else(|| panic!("string {s:?} is not an atom of the vector universe"))
}

fn arr_rank(v: &Val) -> usize {
    shape_arr_atoms()
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
        // Union arms (issue #339), canonical base order — for textual comparison.
        Fact::Union { arms, nullable } => {
            let rendered: Vec<String> = arms
                .iter()
                .map(|(b, r)| match r {
                    Some(r) => format!("{}:{}", render_base(*b), render_refinement(r)),
                    None => render_base(*b).to_owned(),
                })
                .collect();
            format!("U({},{})", rendered.join("|"), render_nullable(*nullable))
        }
        Fact::Shape { shape, nullable } => {
            format!("A({},{})", render_shape(shape), render_nullable(*nullable))
        }
    }
}

// Shape rendering (ADR-0062 S2)

fn key_rank(s: &PhpStr) -> usize {
    KEY_ATOMS
        .iter()
        .position(|a| *s == *a)
        .unwrap_or_else(|| panic!("key {s:?} is not an atom of the vector universe"))
}

fn render_key(k: &Key) -> String {
    match k {
        Key::Int(n) => format!("i{}", render_int(*n)),
        Key::Str(s) => format!("s{}", key_rank(s)),
    }
}

fn render_presence(p: Presence) -> &'static str {
    match p {
        Presence::Required { witnessed: true } => "R!",
        Presence::Required { witnessed: false } => "R",
        Presence::Optional => "O",
        Presence::Absent => "X",
    }
}

fn render_slot(slot: Option<&Fact>) -> String {
    match slot {
        None => "-".to_owned(),
        Some(f) => render_fact(f),
    }
}

fn render_key_class(c: KeyClass) -> &'static str {
    match c {
        KeyClass::Int => "i",
        KeyClass::Str => "s",
        KeyClass::ArrayKey => "k",
    }
}

fn render_tail(t: &Tail) -> String {
    match t {
        Tail::Sealed => ".".to_owned(),
        Tail::Unsealed { key, value } => {
            format!("*{}:{}", render_key_class(*key), render_slot(value.as_deref()))
        }
    }
}

fn render_cover(c: &Cover) -> String {
    let keys: Vec<String> = c.keys.iter().map(render_key).collect();
    let flavor = match c.flavor {
        CoverFlavor::Isset => "i",
        CoverFlavor::KeyExists => "e",
    };
    format!("{}@{}", keys.join("+"), flavor)
}

fn render_shape(s: &ShapeFact) -> String {
    let fields: Vec<String> = s
        .fields
        .iter()
        .map(|(k, p, slot)| {
            format!("{}={}:{}", render_key(k), render_presence(*p), render_slot(slot.as_deref()))
        })
        .collect();
    let covers: Vec<String> = s.covers.iter().map(render_cover).collect();
    format!(
        "{{{}|{}|{}|{}|{}}}",
        if fields.is_empty() { "-".to_owned() } else { fields.join(";") },
        render_tail(&s.tail),
        render_cert(s.is_list),
        if s.non_empty { "ne" } else { "-" },
        if covers.is_empty() { "-".to_owned() } else { covers.join(";") },
    )
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

// The universe — same construction and order as `SteinsDomain.Vectors`.

fn values() -> Vec<Val> {
    let mut out = vec![Val::Null, Val::Bool(false), Val::Bool(true)];
    for i in [i64::MIN, -1, 0, 1, 2, 8, 9, i64::MAX] {
        out.push(Val::Int(i));
    }
    for (f, _) in FLOAT_ATOMS {
        out.push(Val::Float(f));
    }
    for s in STR_ATOMS {
        out.push(Val::Str(s.into()));
    }
    for (a, _) in arr_atoms() {
        out.push(a);
    }
    out
}

/// Predicate sets buildable through `StrPreds`: casing-free seven, plus an eighth
/// (`{NonFalsy, Numeric}` w/o `NonEmpty`) unreachable — `union` closes, `intersect`
/// can't add bits. Casing tail spans (not cross-products): each casing alone/together,
/// vs length, vs falsy/numeric — matches `SteinsDomain.Vectors.predsUniverse` order.
fn preds_universe() -> Vec<StrPreds> {
    vec![
        StrPreds::empty(),
        StrPreds::NON_EMPTY,
        StrPreds::NON_FALSY,
        StrPreds::NUMERIC,
        StrPreds::NON_FALSY.close(),
        StrPreds::NUMERIC.close(),
        StrPreds::NON_FALSY.union(StrPreds::NUMERIC),
        StrPreds::LOWERCASE,
        StrPreds::UPPERCASE,
        StrPreds::LOWERCASE.union(StrPreds::UPPERCASE),
        StrPreds::NON_EMPTY.union(StrPreds::LOWERCASE),
        StrPreds::NON_EMPTY.union(StrPreds::UPPERCASE),
        StrPreds::NON_EMPTY.union(StrPreds::LOWERCASE).union(StrPreds::UPPERCASE),
        // `of("abc")` — non-falsy and lowercase.
        StrPreds::of("abc"),
        // Numeric+cased w/o non-falsy (`'0'`-in-the-set class); via `of("0") ⊓ of("1e5")`.
        StrPreds::NUMERIC.close().union(StrPreds::LOWERCASE),
        StrPreds::NUMERIC.close().union(StrPreds::UPPERCASE),
        // `of("5")` — every predicate but the complement bit, joint-satisfiability witness.
        StrPreds::of("5"),
        // Array-key-cast pair: `of("0")` is falsy decimal-int-string; `of("00")` is the
        // numeric-but-non-canonical near miss; last is ⊥ (both bits via `union`).
        StrPreds::DECIMAL_INT.close(),
        StrPreds::NON_DECIMAL_INT,
        StrPreds::of("00"),
        StrPreds::NON_EMPTY.union(StrPreds::NON_DECIMAL_INT),
        StrPreds::DECIMAL_INT.union(StrPreds::NON_DECIMAL_INT),
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
        vec![Val::Str("0".into()), Val::Str("5".into())],
        vec![Val::Null, Val::Int(1)],
        vec![Val::Bool(false), Val::Bool(true)],
        vec![Val::Int(1), Val::Str("5".into())],
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
        Val::Str(PhpStr::new()),
        Val::Str("5".into()),
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
    // `List.eraseDups`: first wins; dupes come from `Fact::refined` normalising to General.
    let mut deduped: Vec<Fact> = Vec::with_capacity(out.len());
    for f in out {
        if !deduped.contains(&f) {
            deduped.push(f);
        }
    }
    deduped
}

/// Shape seeds, in `shape`-line order. Each is *raw* input to `ShapeFact::normalize`,
/// showing normalization's work: sorting, singleton-cover promotion, sealed-Absent
/// drop, cover antichain, `is_list` recomputation. Rows 0–8: ADR-0062 §3 / RFC #14939
/// `is_list` table. 9–11: A-G1 lowerings (`list<T>`, typed tail, §5 tail-key fixture).
/// 12–18: A-G8 cover laws + remaining invariants.
fn shape_seeds() -> Vec<ShapeFact> {
    let req = Presence::Required { witnessed: true };
    let sint = |i: i64| Some(Box::new(Fact::Singleton(Val::Int(i))));
    let gint = || Some(Box::new(Fact::General { base: Base::Int, nullable: false }));
    let none = || None;
    let seal = |fields: Vec<(Key, Presence, Option<Box<Fact>>)>| {
        ShapeFact::normalize(fields, Tail::Sealed, Certainty::Maybe, false, Vec::new())
    };
    vec![
        // 0  array{}                          — Yes
        seal(vec![]),
        // 1  array{0: 1}                      — Yes
        seal(vec![(ik(0), req, sint(1))]),
        // 2  array{0?: 1}                     — Yes
        seal(vec![(ik(0), Presence::Optional, sint(1))]),
        // 3  array{0: 1, 1: 2}                — Maybe (two realizable orders)
        seal(vec![(ik(0), req, sint(1)), (ik(1), req, sint(2))]),
        // 4  array{a?: 1}                     — Maybe
        seal(vec![(sk(0), Presence::Optional, sint(1))]),
        // 5  array{a: 1}                      — No (required string key)
        seal(vec![(sk(0), req, sint(1))]),
        // 6  array{1: 2}                      — No (gapped required int key)
        seal(vec![(ik(1), req, sint(2))]),
        // 7  array{0?: 1, 1: 2}               — Maybe (the gap is fillable)
        seal(vec![(ik(0), Presence::Optional, sint(1)), (ik(1), req, sint(2))]),
        // 8  array{-1: 2}                     — No (negative key)
        seal(vec![(ik(-1), req, sint(2))]),
        // 9  array (the degenerate shape)     — Maybe
        ShapeFact::plain_array(),
        // 10 list<int>: typed tail + the caller's Yes sharpening Maybe
        ShapeFact::normalize(
            Vec::new(),
            Tail::Unsealed { key: KeyClass::Int, value: gint() },
            Certainty::Yes,
            false,
            Vec::new(),
        ),
        // 11 array{a: int, ...<string, int>}  — the §5 tail-key fixture
        ShapeFact::normalize(
            vec![(sk(0), req, gint())],
            Tail::Unsealed { key: KeyClass::Str, value: gint() },
            Certainty::Maybe,
            false,
            Vec::new(),
        ),
        // 12 array{1: 2, ...<string, mixed>}  — No: string tail can't fill gap at key 0
        ShapeFact::normalize(
            vec![(ik(1), req, sint(2))],
            Tail::Unsealed { key: KeyClass::Str, value: none() },
            Certainty::Maybe,
            false,
            Vec::new(),
        ),
        // 13/14: Isset cover over {a, b}, then the same keys with a KeyExists cover
        ShapeFact::normalize(
            vec![(sk(0), Presence::Optional, none()), (sk(1), Presence::Optional, none())],
            Tail::Sealed,
            Certainty::Maybe,
            false,
            vec![Cover::new(vec![sk(1), sk(0)], CoverFlavor::Isset)],
        ),
        ShapeFact::normalize(
            vec![(sk(0), Presence::Optional, none()), (sk(1), Presence::Optional, none())],
            Tail::Sealed,
            Certainty::Maybe,
            false,
            vec![Cover::new(vec![sk(0), sk(1)], CoverFlavor::KeyExists)],
        ),
        // 15 a singleton cover promotes to presence rather than surviving
        ShapeFact::normalize(
            vec![(sk(0), Presence::Optional, sint(1))],
            Tail::Sealed,
            Certainty::Maybe,
            false,
            vec![Cover::new(vec![sk(0)], CoverFlavor::Isset)],
        ),
        // 16 supersets drop: {a,b,c} loses to {a,b}
        ShapeFact::normalize(
            vec![
                (sk(0), Presence::Optional, none()),
                (sk(1), Presence::Optional, none()),
                (sk(2), Presence::Optional, none()),
            ],
            Tail::Sealed,
            Certainty::Maybe,
            false,
            vec![
                Cover::new(vec![sk(0), sk(1), sk(2)], CoverFlavor::KeyExists),
                Cover::new(vec![sk(1), sk(0)], CoverFlavor::KeyExists),
            ],
        ),
        // 17 a proven-Absent key survives an unsealed tail
        ShapeFact::normalize(
            vec![(sk(0), Presence::Absent, none()), (sk(1), Presence::Absent, none())],
            Tail::Unsealed { key: KeyClass::ArrayKey, value: none() },
            Certainty::Maybe,
            false,
            Vec::new(),
        ),
        // 18 non-empty with only a string optional — No, and `[]` is excluded
        ShapeFact::normalize(
            vec![(sk(0), Presence::Optional, sint(1))],
            Tail::Sealed,
            Certainty::Maybe,
            true,
            Vec::new(),
        ),
    ]
}

/// Shape section's `Fact` universe: four facts (one nullable) plus rejected neighbours.
fn shape_facts() -> Vec<Fact> {
    let seeds = shape_seeds();
    let sh = |i: usize, nullable: bool| Fact::Shape {
        shape: Box::new(seeds[i].clone()),
        nullable,
    };
    vec![
        sh(1, false),
        sh(5, false),
        sh(9, false),
        sh(9, true),
        Fact::Singleton(arr_atom(0)),
        Fact::Singleton(arr_atom(2)),
        Fact::Singleton(Val::Null),
        Fact::Singleton(Val::Int(1)),
        Fact::OneOf(vec![arr_atom(0), arr_atom(1)]),
        Fact::OneOf(vec![Val::Null, arr_atom(1)]),
        Fact::OneOf(vec![Val::Null, Val::Int(1)]),
        Fact::General { base: Base::Int, nullable: false },
    ]
}

/// The descent seeds: value sets that overflow `CAP` with arrays in them.
fn descent_seeds() -> Vec<(&'static str, Vec<Val>)> {
    let arrays: Vec<Val> = (6..=14).map(arr_atom).collect();
    let mut with_null = arrays.clone();
    with_null.push(Val::Null);
    let mut mixed = arrays.clone();
    mixed.push(Val::Int(1));
    let lists: Vec<Val> = vec![
        arr_atom(0),
        arr_atom(1),
        arr_atom(2),
        arr_atom(3),
        arr_atom(4),
        arr_atom(5),
        arr_atom(6),
        arr_atom(7),
        arr_atom(8),
    ];
    vec![
        ("allarrays", arrays),
        ("withnull", with_null),
        ("mixed", mixed),
        ("assorted", lists),
    ]
}

/// `None` is ⊤ and absorbs — what `join_envs` does when it drops a binding.
fn join_opt(a: Option<&Fact>, b: Option<&Fact>) -> Option<Fact> {
    match (a, b) {
        (Some(x), Some(y)) => x.join(y),
        _ => None,
    }
}

// Generation

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

    // Shape section (ADR-0062 S2), appended so earlier lines stay untouched.
    let atoms = shape_arr_atoms();
    for (rank, (v, lit)) in atoms.iter().enumerate() {
        let entries = match v {
            Val::Array(e) => e.as_slice(),
            other => panic!("atom {rank} is not an array: {other:?}"),
        };
        out.push(format!(
            "shapearr arr#{rank} {lit} {}",
            if array_is_list(entries) { "list" } else { "nolist" }
        ));
    }

    let seeds = shape_seeds();
    for (i, s) in seeds.iter().enumerate() {
        out.push(format!("shape {i} => {}", render_shape(s)));
    }
    for (i, s) in seeds.iter().enumerate() {
        for rank in 0..atoms.len() {
            out.push(format!(
                "shapeadmits {i} arr#{rank} => {}",
                s.admits(&arr_entries(rank))
            ));
        }
    }
    for (i, a) in seeds.iter().enumerate() {
        for (j, b) in seeds.iter().enumerate() {
            out.push(format!("shapejoin {i} {j} => {}", render_shape(&a.join(b))));
        }
    }
    for rank in 0..atoms.len() {
        out.push(format!(
            "shapelift arr#{rank} => {}",
            render_shape(&ShapeFact::lift(&arr_entries(rank)))
        ));
    }

    let sfs = shape_facts();
    for (i, f) in sfs.iter().enumerate() {
        out.push(format!("shapefact {i} => {}", render_fact(f)));
    }
    for (i, f) in sfs.iter().enumerate() {
        for rank in 0..atoms.len() {
            out.push(format!("shapefactadmits {i} arr#{rank} => {}", f.admits(&arr_atom(rank))));
        }
        out.push(format!("shapefactadmits {i} null => {}", f.admits(&Val::Null)));
        out.push(format!("shapefactadmits {i} int:1 => {}", f.admits(&Val::Int(1))));
    }
    for (i, a) in sfs.iter().enumerate() {
        for (j, b) in sfs.iter().enumerate() {
            out.push(format!("shapefactjoin {i} {j} => {}", render_opt_fact(a.join(b).as_ref())));
        }
    }
    for f in &sfs {
        out.push(format!("shapetruthy {} => {}", render_fact(f), render_cert(f.truthy())));
        out.push(format!("shapeisnull {} => {}", render_fact(f), render_cert(f.is_null())));
    }
    for (name, vals) in descent_seeds() {
        out.push(format!(
            "shapedescent {name} => {}",
            render_opt_fact(Fact::from_vals(vals).as_ref())
        ));
    }

    // ADR-0062 S4: `count_range` (S3 Lean debt) + four narrowing operators, per seed.
    for (i, s) in seeds.iter().enumerate() {
        out.push(format!("shapecount {i} => {}", render_range(s.count_range())));
    }
    for (i, s) in seeds.iter().enumerate() {
        for k in op_keys() {
            out.push(format!(
                "shapepromote {i} {} isset => {}",
                render_key(&k),
                render_shape(&s.promote_present(&k, true, true))
            ));
            out.push(format!(
                "shapepromote {i} {} exists => {}",
                render_key(&k),
                render_shape(&s.promote_present(&k, false, true))
            ));
        }
    }
    for (i, s) in seeds.iter().enumerate() {
        for k in op_keys() {
            out.push(format!(
                "shapeabsent {i} {} => {}",
                render_key(&k),
                render_shape(&s.mark_absent(&k))
            ));
        }
    }
    for (i, s) in seeds.iter().enumerate() {
        out.push(format!("shapenonempty {i} => {}", render_shape(&s.set_non_empty())));
    }
    for (i, s) in seeds.iter().enumerate() {
        for c in [Certainty::Yes, Certainty::No] {
            out.push(format!(
                "shapeislist {i} {} => {}",
                render_cert(c),
                render_shape(&s.set_is_list(c))
            ));
        }
    }

    // ADR-0062 S5: cover recording (A-G8) and the discharge query (A-G11).
    for (i, s) in seeds.iter().enumerate() {
        for (k1, k2) in cover_pairs() {
            for fl in [CoverFlavor::Isset, CoverFlavor::KeyExists] {
                out.push(format!(
                    "shaperecordcover {i} {} {} {} => {}",
                    render_key(&k1),
                    render_key(&k2),
                    render_flavor(fl),
                    render_shape(&s.record_cover(vec![k1.clone(), k2.clone()], fl))
                ));
            }
        }
    }
    for (i, s) in seeds.iter().enumerate() {
        for (k1, k2) in cover_pairs() {
            for fl in [CoverFlavor::Isset, CoverFlavor::KeyExists] {
                let covered = s.record_cover(vec![k1.clone(), k2.clone()], fl);
                out.push(format!(
                    "shapecoverproves {i} {} {} {} => {}",
                    render_key(&k1),
                    render_key(&k2),
                    render_flavor(fl),
                    covered
                        .cover_proves(&k2, std::slice::from_ref(&k1))
                        .map_or("-", render_flavor)
                ));
            }
        }
    }


    // Array-stratum soundness: `γ(a) ∪ γ(b) ⊆ γ(a ⊔ b)`, lift admits what it lifted,
    // descent admits every member — checked exhaustively, like `assoc` (spec's REPORT.md).
    let mut total = 0usize;
    let mut violations = 0usize;
    for a in &seeds {
        for b in &seeds {
            let joined = a.join(b);
            for rank in 0..atoms.len() {
                let e = arr_entries(rank);
                total += 1;
                if (a.admits(&e) || b.admits(&e)) && !joined.admits(&e) {
                    violations += 1;
                }
            }
        }
    }
    out.push(format!("shapejoinsound {total} {violations}"));

    let mut total = 0usize;
    let mut violations = 0usize;
    for rank in 0..atoms.len() {
        let e = arr_entries(rank);
        total += 1;
        if !ShapeFact::lift(&e).admits(&e) {
            violations += 1;
        }
    }
    out.push(format!("shapeliftsound {total} {violations}"));

    let mut total = 0usize;
    let mut violations = 0usize;
    for (_, vals) in descent_seeds() {
        if let Some(f) = Fact::from_vals(vals.clone()) {
            for v in &vals {
                total += 1;
                if !f.admits(v) {
                    violations += 1;
                }
            }
        }
    }
    out.push(format!("shapedescentsound {total} {violations}"));

    let probe: Vec<Val> = (0..atoms.len())
        .map(arr_atom)
        .chain([Val::Null, Val::Int(1)])
        .collect();
    let mut total = 0usize;
    let mut violations = 0usize;
    for a in &sfs {
        for b in &sfs {
            let joined = a.join(b);
            for v in &probe {
                total += 1;
                if (a.admits(v) || b.admits(v))
                    && !joined.as_ref().is_none_or(|g| g.admits(v))
                {
                    violations += 1;
                }
            }
        }
    }
    out.push(format!("shapefactjoinsound {total} {violations}"));

    // S4 narrowing law: everything the receiver admits and satisfies the guard survives.
    let mut total = 0usize;
    let mut violations = 0usize;
    for s in &seeds {
        for rank in 0..atoms.len() {
            let e = arr_entries(rank);
            if !s.admits(&e) {
                continue;
            }
            for k in op_keys() {
                match e.iter().find(|(ek, _)| *ek == k).map(|(_, v)| v) {
                    None => {
                        total += 1;
                        if !s.mark_absent(&k).admits(&e) {
                            violations += 1;
                        }
                    }
                    Some(v) => {
                        total += 1;
                        if !s.promote_present(&k, false, true).admits(&e) {
                            violations += 1;
                        }
                        if *v != Val::Null {
                            total += 1;
                            if !s.promote_present(&k, true, true).admits(&e) {
                                violations += 1;
                            }
                        }
                    }
                }
            }
            if !e.is_empty() {
                total += 1;
                if !s.set_non_empty().admits(&e) {
                    violations += 1;
                }
            }
            total += 1;
            if !s.set_is_list(Certainty::from_bool(array_is_list(&e))).admits(&e) {
                violations += 1;
            }
        }
    }
    out.push(format!("shapenarrowsound {total} {violations}"));

    // `mark_absent`'s 2nd law (`unset($x[k])`): admits `v \ {k}` for every admitted `v`.
    let mut total = 0usize;
    let mut violations = 0usize;
    for s in &seeds {
        for rank in 0..atoms.len() {
            let e = arr_entries(rank);
            if !s.admits(&e) {
                continue;
            }
            for k in op_keys() {
                let erased: Vec<(Key, Val)> =
                    e.iter().filter(|(ek, _)| *ek != k).cloned().collect();
                total += 1;
                if !s.mark_absent(&k).admits(&erased) {
                    violations += 1;
                }
            }
        }
    }
    out.push(format!("shapeunsetsound {total} {violations}"));

    // `count_range` bounds every admitted array's entry count (the S3 debt).
    let mut total = 0usize;
    let mut violations = 0usize;
    for s in &seeds {
        for rank in 0..atoms.len() {
            let e = arr_entries(rank);
            if !s.admits(&e) {
                continue;
            }
            total += 1;
            if !s.count_range().contains(i64::try_from(e.len()).expect("small")) {
                violations += 1;
            }
        }
    }
    out.push(format!("shapecountsound {total} {violations}"));

    // S5 recording law: satisfying disjunction survives recording (can't lose a member).
    let mut total = 0usize;
    let mut violations = 0usize;
    for s in &seeds {
        for (k1, k2) in cover_pairs() {
            for rank in 0..atoms.len() {
                let e = arr_entries(rank);
                if !s.admits(&e) {
                    continue;
                }
                let entry = |k: &Key| e.iter().find(|(ek, _)| ek == k).map(|(_, v)| v);
                for fl in [CoverFlavor::Isset, CoverFlavor::KeyExists] {
                    let sat = [&k1, &k2].into_iter().any(|k| match entry(k) {
                        None => false,
                        Some(v) => fl == CoverFlavor::KeyExists || *v != Val::Null,
                    });
                    if !sat {
                        continue;
                    }
                    total += 1;
                    if !s.record_cover(vec![k1.clone(), k2.clone()], fl).admits(&e) {
                        violations += 1;
                    }
                }
            }
        }
    }
    out.push(format!("shapecoversound {total} {violations}"));

    // A-G11 law: `cover_proves` answering means the key is present after fallthrough.
    let mut total = 0usize;
    let mut violations = 0usize;
    for s in &seeds {
        for (k1, k2) in cover_pairs() {
            for fl in [CoverFlavor::Isset, CoverFlavor::KeyExists] {
                let covered = s.record_cover(vec![k1.clone(), k2.clone()], fl);
                for rank in 0..atoms.len() {
                    let e = arr_entries(rank);
                    if !covered.admits(&e) {
                        continue;
                    }
                    let entry = |k: &Key| e.iter().find(|(ek, _)| ek == k).map(|(_, v)| v);
                    let Some(g) = covered.cover_proves(&k2, std::slice::from_ref(&k1)) else {
                        continue;
                    };
                    let fell_through = match entry(&k1) {
                        None => true,
                        Some(v) => g == CoverFlavor::Isset && *v == Val::Null,
                    };
                    if !fell_through {
                        continue;
                    }
                    let ok = match entry(&k2) {
                        None => false,
                        Some(v) => g == CoverFlavor::KeyExists || *v != Val::Null,
                    };
                    total += 1;
                    if !ok {
                        violations += 1;
                    }
                }
            }
        }
    }
    out.push(format!("shapedischargesound {total} {violations}"));
    out
}

fn expected() -> Vec<String> {
    let raw = include_str!("fixtures/lean-vectors.expected");
    raw.lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

// The check

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

/// Fixture-data tally; assert directly: `join` is associative over the vector universe.
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

/// Array-stratum soundness is *checked*, not proved in Lean (`spike/lean-domain/REPORT.md`).
/// Fixture-data tallies; pin zero failures — join loses no member, lift and descent admit.
#[test]
fn the_array_stratum_loses_no_member_over_the_vector_universe() {
    let lines = generate();
    for id in [
        "shapejoinsound",
        "shapeliftsound",
        "shapedescentsound",
        "shapefactjoinsound",
        "shapenarrowsound",
        "shapeunsetsound",
        "shapecountsound",
        "shapecoversound",
        "shapedischargesound",
    ] {
        let line = lines
            .iter()
            .find(|l| l.starts_with(&format!("{id} ")))
            .unwrap_or_else(|| panic!("{id} line"));
        let mut parts = line.split_whitespace().skip(1);
        let total: usize = parts.next().expect("total").parse().expect("number");
        let violations: usize = parts.next().expect("violations").parse().expect("number");
        assert!(total > 0, "{id} checked nothing");
        assert_eq!(violations, 0, "{id}: the array stratum lost a member ({line})");
    }
}
