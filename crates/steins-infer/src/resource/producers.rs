//! The resource producers (ADR-0056 §8, ADR-0098 §2.2): the arms and the heap resource
//! a producer call's return binds ([`builtin_resource_arms`]), and the element places a
//! producer mints — `stream_socket_pair`'s pair from its return, and `proc_open`'s pipes
//! through its `$pipes` out-parameter, at a statement or a guard.

use std::collections::HashMap;

use steins_catalog::ResourceKind;
use steins_contract::{ContractTy, ResourceState};
use steins_domain::Key as VKey;
use steins_syntax::{ArgValue, CallExpr, CondExpr, StmtKind};

use crate::cx::Cx;
use crate::env::{ContractArm, HandleState, HeapRes, Known, Store, Stratum};
use crate::fold::Folder;
use crate::out_params::out_param_seed_callee;
use crate::refine::collect_truthy_calls;
use crate::walk::WalkCx;

/// The **resource-return arms** of a builtin call (ADR-0056 §8): `resource` plus,
/// where the stub declares one, the `false` failure arm — both `Verified` — and
/// the **heap resource** the assignment binds beside them (ADR-0097 §2.3):
/// the row's kind, in the `Open` state, with the producer's name.
///
/// # Why these arms are `Verified` when the declared floor's are not
///
/// ADR-0069's floor is `Asserted` because a `functionMap` row is an unconfirmed
/// claim about the analyzing PHP. These rows cannot disagree with the engine in
/// that way: [`Folder::builtin_resource_return`] admits the row only while this
/// engine declares NO return type for the name. A migrated function
/// (`curl_init` → `CurlHandle|false`) declares one and is refused; a genuine
/// resource producer declares none because the language has no syntax for it.
///
/// The resource arm's declared state is [`ResourceState::Any`]: the lane
/// carries the type, and `Open` is the heap entry's claim — a proof, since the
/// producer returned and nothing has touched the handle yet; the `false` arm,
/// where present, is what the ordinary `=== false` guard subtracts before
/// [`store_holds_resource`] admits the lane at all.
///
/// The project-shadowing check comes first, as for the floor.
///
/// [`store_holds_resource`]: crate::resource::store_holds_resource
pub(crate) fn builtin_resource_arms(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
) -> Option<(Vec<ContractArm>, HeapRes)> {
    if cx.index.has_simple_function(name) {
        return None;
    }
    let row = folder.builtin_resource_return(name)?;
    let mut arms = vec![ContractArm {
        ty: ContractTy::Resource { state: ResourceState::Any },
        stratum: Stratum::Verified,
    }];
    if row.may_be_false {
        arms.push(ContractArm { ty: ContractTy::LitBool(false), stratum: Stratum::Verified });
    }
    let res = HeapRes {
        kind: row.kind,
        state: HandleState::Open,
        producer: name.trim_start_matches('\\').to_ascii_lowercase(),
    };
    Some((arms, res))
}

// ---------------------------------------------------------------------------
// The array-of-handles producers (ADR-0098 §4 slice 2)
// ---------------------------------------------------------------------------

/// The contract arms an **element place** a producer minted carries
/// (ADR-0098 §2.2): one `resource` arm, `Verified`, and no failure arm.
///
/// The failure arm belongs to the *container*, never to an element: PHP either
/// hands back the array of handles or hands back nothing at all, and when it
/// hands one back every entry in it is an open handle. So the lane that
/// ADR-0056 §8.6's lock reads is already narrowed at the moment the place is
/// bound, which is the difference between a minted place and one
/// [`bind_handle_elements`] copied off a variable that still had to be guarded.
///
/// [`bind_handle_elements`]: crate::assign::bind_handle_elements
fn produced_place_arms() -> Vec<ContractArm> {
    vec![ContractArm {
        ty: ContractTy::Resource { state: ResourceState::Any },
        stratum: Stratum::Verified,
    }]
}

/// The heap resource one produced element holds: a **fresh** allocation, `Open`,
/// of the producer's kind. Fresh and not shared — `$pair[0]` and `$pair[1]` are
/// two different handles with two different ids (probed at 8.5.10:
/// `get_resource_id()` answers 4 and 5 for one pair, and `fclose($pair[0])`
/// leaves `is_resource($pair[1])` true), so one id for both would make closing
/// either close the other.
fn produced_handle(producer: &str, kind: ResourceKind) -> HeapRes {
    HeapRes { kind, state: HandleState::Open, producer: producer.to_ascii_lowercase() }
}

/// `stream_socket_pair`'s name, spelled once.
const SOCKET_PAIR: &str = "stream_socket_pair";

/// The return declaration `stream_socket_pair` must still carry for its row to
/// be admitted — the ADR-0056 §8.2 tripwire in the one shape it can take for a
/// producer whose return type PHP *can* spell.
///
/// `fopen`'s tripwire is silence: the engine declaring anything at all disowns
/// the row. That reading is unavailable here, because `array|false` is exactly
/// what this engine declares and always has. So the tripwire is the declaration
/// itself: the day a PHP hands back `Socket[]|false`, an `iterable`, or a pair
/// object, the string below stops matching and the row switches itself off with
/// no denylist and no release of Steins.
const SOCKET_PAIR_RETURN: &str = "array|false";

/// The places `$pair = stream_socket_pair(…)` binds (ADR-0098 §2.2): **exactly
/// two**, keyed `0` and `1`, each a fresh `Open` `stream` handle.
///
/// Probed at 8.5.10 — the count is the contract and not an observation of one
/// call:
///
/// ```text
/// $p = stream_socket_pair(STREAM_PF_UNIX, STREAM_SOCK_STREAM, 0);
///   gettype($p) === 'array'   array_keys($p) === [0, 1]   count($p) === 2
///   get_debug_type($p[0]) === get_debug_type($p[1]) === 'resource (stream)'
///   get_resource_id($p[0]) === 4   get_resource_id($p[1]) === 5   ($p[0] !== $p[1])
///   array_is_list($p) === true
/// $p = stream_socket_pair(STREAM_PF_UNIX, STREAM_SOCK_DGRAM, 0);   keys [0, 1] too
/// $p = @stream_socket_pair(-1, -1, -1);   === false   (the whole failure shape)
/// fclose($p[0]);   is_resource($p[0]) === false   is_resource($p[1]) === true
/// ```
///
/// The failure arm is `false`, never a shorter array: there is no call that
/// answers a one-element list, so "the array exists" and "both places hold an
/// open handle" are the same statement, and `pair[0]` needs no guard of its own
/// beyond the ordinary `=== false` on `$pair`.
///
/// Two gates, both the ones the `fopen` rung applies: a project function of the
/// same simple name shadows the builtin and answers instead, and the engine's
/// own declaration must still be [`SOCKET_PAIR_RETURN`]. Without a live sidecar
/// there is no declaration to read and nothing is bound — the sound subset
/// (ADR-0004), same as every other resource rung.
pub(crate) fn socket_pair_places(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
) -> Option<Vec<(VKey, HeapRes)>> {
    if !name.eq_ignore_ascii_case(SOCKET_PAIR) || cx.index.has_simple_function(name) {
        return None;
    }
    let declared = folder.builtin_return_type(name)?;
    if !declared.eq_ignore_ascii_case(SOCKET_PAIR_RETURN) {
        return None;
    }
    Some(vec![
        (VKey::Int(0), produced_handle(SOCKET_PAIR, ResourceKind::Stream)),
        (VKey::Int(1), produced_handle(SOCKET_PAIR, ResourceKind::Stream)),
    ])
}

/// `proc_open`'s name, spelled once.
const PROC_OPEN: &str = "proc_open";

/// One descriptor word `proc_open` accepts, spelled the way php-src compares it
/// — **byte for byte**. `'PIPE'` and `'Pipe'` are not this word (probed at
/// 8.5.10: `proc_open(): PIPE is not a valid descriptor spec/mode`, the call
/// answers `false` and leaves `$pipes` untouched), so a case-insensitive
/// comparison here would mint a place for a call that never writes one — on
/// EVERY execution, not on a failure path.
struct DescriptorWord {
    /// The word itself, lowercase, as php-src's `zend_string_equals_literal`
    /// sees it.
    word: &'static str,
    /// Whether `proc_open` hands back an entry in `$pipes` at this descriptor's
    /// key — and, for the two words where it does but this rung stays out, see
    /// [`proc_open_places`]' "two descriptor words" section.
    binds_place: bool,
    /// The cells **after** `0` the engine demands beside this word. Each absent
    /// one is a `ValueError` raised before `proc_open` returns anything, so a
    /// descriptor missing one never reaches a statement that could read a place.
    /// Probed at 8.5.10, one message per cell: `Missing mode parameter for
    /// 'pipe'`, `Missing file name parameter for 'file'`, `Missing mode
    /// parameter for 'file'`, `Missing redirection target`.
    cells: &'static [i64],
}

/// **Every** descriptor word this rung will read a spec through, and no others.
///
/// A word outside this table refuses the **whole** spec, for the reason `PIPE`
/// does: php-src warns `… is not a valid descriptor spec/mode`, the call answers
/// `false`, and `$pipes` is left exactly as it was found — so no key of that
/// spec is ever written, the readable ones included. `pty` is deliberately
/// outside: it was probed to produce an entry *here*, but it needs a build whose
/// `proc_open` has pseudo-terminal support, and a build without it cannot both
/// refuse the word and write the other keys. Admitting it is a probe on such a
/// build, not a reading of this comment.
const DESCRIPTOR_WORDS: &[DescriptorWord] = &[
    DescriptorWord { word: "pipe", binds_place: true, cells: &[1] },
    DescriptorWord { word: "file", binds_place: false, cells: &[1, 2] },
    DescriptorWord { word: "null", binds_place: false, cells: &[] },
    DescriptorWord { word: "redirect", binds_place: false, cells: &[1] },
    DescriptorWord { word: "socket", binds_place: false, cells: &[] },
];

/// The row for `word`, matched **case-sensitively**, or `None` for a word
/// php-src does not accept — which is a refusal of the whole spec.
fn descriptor_word(word: &[u8]) -> Option<&'static DescriptorWord> {
    DESCRIPTOR_WORDS.iter().find(|d| d.word.as_bytes() == word)
}

/// The places `proc_open($cmd, $spec, $pipes, …)` binds (ADR-0098 §2.2): **one
/// per `pipe` descriptor of `$spec`, at that descriptor's own key** — never one
/// per position and never one per argument.
///
/// The key set is a function of the spec, which is the whole reason this cannot
/// be a fixed row. Probed at 8.5.10:
///
/// ```text
/// [0 => ['pipe','r'], 2 => ['pipe','w']]            array_keys($pipes) === [0, 2]
/// [0 => ['file','/dev/null','r'], 1 => ['pipe','w']]                    === [1]
/// [0 => ['pipe','r'], 1 => ['file','/dev/null','w']]                    === [0]
/// [0 => ['pipe','r'], 5 => ['pipe','w']]                                === [0, 5]
/// [3 => ['pipe','r']]                                                   === [3]
/// [['pipe','r'], ['pipe','w']]                                          === [0, 1]
/// [100 => ['pipe','r']]                                                 === [100]
/// [1 => ['null']]                                                       === []
/// [1 => ['pipe','w'], 2 => ['redirect', 1]]                             === [1]
/// [1 => $fh]           (a stream resource as the descriptor)            === []
/// []                                                                    === []
/// ```
///
/// Every entry is `resource (stream)` — `process` is the *return's* kind, not
/// the pipes' — and each is closed by `fclose`, after which the element keeps
/// holding the closed handle (`gettype($pipes[1])` reads `resource (closed)`).
///
/// # Two descriptor words that also produce an entry, and stay out anyway
///
/// `['socket']` and `['pty']` were probed here too, and both yield an entry
/// (`[1 => ['socket']]` → key `1`; `[0 => ['pty'], 1 => ['pty']]` → keys `0` and
/// `1`). Neither is admitted, and they are held back differently because PHP
/// treats them differently. `socket` is a word php-src accepts on any build, so
/// a spec that uses one stays readable and the key it fills is simply not
/// claimed — a missed finding, which is where the family already is. `pty`
/// needs a build whose `proc_open` has pseudo-terminal support, which this probe
/// cannot speak for, so it is outside [`DESCRIPTOR_WORDS`] and refuses the whole
/// spec: on a build that lacks the support the call cannot succeed, and the
/// other keys of that same spec would be places nothing ever wrote.
///
/// # What is checked, in the order it is checked
///
/// 1. the spec resolves to a **literal array** — else nothing is bound;
/// 2. every key is an **integer**. A string key is `ValueError: proc_open():
///    Argument #2 ($descriptor_spec) must be an integer indexed array` (probed),
///    so the call raises before it returns and no statement after it runs;
/// 3. every key is **non-negative**. Probed at 8.5.10, for every descriptor word
///    alike: `[-1 => ['pipe','r']]` warns `Unable to copy file descriptor 5 (for
///    pipe) into file descriptor -1: Bad file descriptor`, answers `false` and
///    leaves `$pipes` untouched — on every execution, so a place minted from
///    such a spec could never be read;
/// 4. every descriptor is a **literal array** whose cell `0` is a **literal
///    string** — a word this walk cannot name is not a non-`pipe`;
/// 5. that word is one [`DESCRIPTOR_WORDS`] carries, compared **byte for
///    byte** — the one php-src compares. Anything else is the `PIPE` case: a
///    warning, `false`, and an untouched `$pipes`;
/// 6. the cells php-src demands beside the word are **present**
///    ([`DescriptorWord::cells`]) — `['pipe']` with no mode is `ValueError:
///    Missing mode parameter for 'pipe'` (probed), which returns nothing at all.
///    Only presence is checked: the mode's *value* is not validated by the
///    engine either (probed: `['pipe','zzz']` and `['pipe', 5]` both open a
///    pipe).
///
/// # And every refusal is refused whole
///
/// A spec the walk cannot prove is a spec whose key set is unknown, and an
/// unknown key set cannot be bound *in part*: the unprovable entry may itself be
/// a `pipe`, so binding the provable ones would claim `$pipes` has exactly the
/// keys this returned — a claim about the entries as a set, which is the thing
/// that was not proven. Legs 2, 3, 5 and 6 refuse whole for a second reason on
/// top of that one: each of them is a spec on which `proc_open` writes
/// **nothing**, every time it runs, so a place bound beside it would be a
/// finding on a line that is never reached.
fn proc_open_places(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
    spec: &ArgValue,
    env: &HashMap<String, Known>,
    poisoned: bool,
) -> Option<Vec<(VKey, HeapRes)>> {
    if !name.eq_ignore_ascii_case(PROC_OPEN) || cx.index.has_simple_function(name) {
        return None;
    }
    // The producer row's own gate (ADR-0056 §8.2), which `proc_open` is already
    // in: the engine has the name, declares no return type for it, and the
    // project minor is the catalog pin. It answers about the *return*, and it
    // is the right gate for the pipes too — a `proc_open` migrated to an object
    // is a `proc_open` whose `$pipes` this probe no longer speaks for.
    folder.builtin_resource_return(name)?;
    // The by-reference tripwire: position 2 is where the handles land, and a
    // position the engine reports by value is one this call cannot write.
    let params = folder.builtin_param_types(name)?;
    if !params.get(PROC_OPEN_PIPES).is_some_and(|p| p.by_ref) {
        return None;
    }
    let Some(ArgValue::Array(items)) = cx.resolve_literal(spec, env, poisoned, folder) else {
        return None;
    };
    let normalized = steins_syntax::normalize_array(&items)?;
    let mut places = Vec::new();
    for (key, descriptor) in normalized {
        // Leg 2: a string key is the `ValueError` above; an integer key can be a
        // place. Leg 3: a negative one never is — the call fails before writing.
        let steins_syntax::NormKey::Int(index) = key else { return None };
        if index < 0 {
            return None;
        }
        // Leg 4. The descriptor's word is its element `0`. A descriptor whose
        // first cell is not a literal string is one whose word is unknown — not
        // a non-`pipe`, which is why it refuses the whole spec rather than
        // contributing nothing.
        let ArgValue::Array(cells) = descriptor else { return None };
        let cells = steins_syntax::normalize_array(&cells)?;
        let word = match cells.iter().find(|(k, _)| *k == steins_syntax::NormKey::Int(0)) {
            Some((_, ArgValue::Str(s))) => s.clone(),
            // No cell `0` at all is a `ValueError` out of the engine
            // (`Missing handle qualifier in array`), so it never reaches a
            // statement that could read a place; refused here regardless.
            _ => return None,
        };
        // Leg 5, byte for byte, and leg 6 beside it.
        let row = descriptor_word(word.as_bytes())?;
        let present =
            |cell: &i64| cells.iter().any(|(k, _)| *k == steins_syntax::NormKey::Int(*cell));
        if !row.cells.iter().all(present) {
            return None;
        }
        if row.binds_place {
            places.push((VKey::Int(index), produced_handle(PROC_OPEN, ResourceKind::Stream)));
        }
    }
    Some(places)
}

/// The 0-based position of `proc_open`'s `&$pipes` — the same index the
/// catalog's [`steins_catalog::out_params`] row carries, spelled here because
/// the two tripwires above read it directly.
const PROC_OPEN_PIPES: usize = 2;

/// Bind `places` as element places of `var` (ADR-0098 §2.2), each to an
/// allocation **this walk mints**.
///
/// The producer twin of [`bind_handle_elements`], and the one line of it that
/// differs is the id: a literal shares the id the source variable holds, a
/// producer has no source to share with and every element is its own handle. So
/// each place gets a [`WalkCx::fresh_id`] of its own, and closing one leaves the
/// others exactly where they were — which is what PHP does (probed at 8.5.10:
/// `fclose($pair[0])` leaves `is_resource($pair[1])` true).
///
/// The caller has already dropped `var`'s previous places; this only adds.
///
/// [`bind_handle_elements`]: crate::assign::bind_handle_elements
pub(crate) fn bind_produced_places(
    w: &WalkCx,
    var: &str,
    places: Vec<(VKey, HeapRes)>,
    store: &mut Store,
) {
    for (key, res) in places {
        let place = crate::env::elem_place(var, &key);
        store.bind_resource(&place, w.fresh_id(), res);
        store.contract.insert(place, produced_place_arms());
    }
}

/// The **out-parameter place seeds** a statement carries (ADR-0098 §2.2): the
/// element places `proc_open($cmd, $spec, $pipes, …)` fills, read on the
/// **pre-call** store and applied by [`apply_produced_places`] after the
/// statement's by-reference invalidation has forgotten `$pipes` — the same two
/// halves, in the same order, that [`stmt_out_param_seeds`] is split into, and
/// for the same reason (ADR-0077 §3.4).
///
/// The statement and the assignment `$p = proc_open(…)` both seed, because the
/// name the assignment rebinds (`$p`, the process handle) is never the name the
/// write lands in (`$pipes`).
///
/// # Why the statement position and not only the guard
///
/// The catalog's witness for this row is [`WrittenWhen::ReturnTruthy`] (probed:
/// a `proc_open` that answers `false` leaves `$pipes` byte for byte as it found
/// it), and a bare statement proves only that the call returned. That is exactly
/// the rule that keeps `preg_match`'s `$matches` **fact** out of this position —
/// a fact stated on a path the callee never wrote is a fact about a value that
/// never existed.
///
/// A place is not a fact, and its soundness argument is ADR-0097 §2.4's, which
/// this position already rests on everywhere else: **a closing call's return is
/// the whole premise.** The only way a place here reaches a finding is a closing
/// call on it, and on the path where `proc_open` answered `false` that call is a
/// `TypeError` — `$pipes` is untouched, `$pipes[1]` is not a handle, and
/// `fclose()` of a non-resource throws (ADR-0097 §1.1). The statement after it
/// therefore runs only on the path where the write did happen, which is the
/// path the place describes.
///
/// ## What the premise depends on, and what would break it
///
/// The premise reads in full as *a closing call either closes what it was
/// handed or does not return*. PHP has **one** exception to it, and the
/// deferral above is standing on the fact that Steins cannot reach it. Probed at
/// 8.5.10:
///
/// ```text
/// $d = opendir('/tmp');   fclose($d);
///   → Warning: fclose(): cannot close the provided stream, as it must not be
///     manually closed;  returns false;  gettype($d) is still 'resource'
///     (and get_debug_type($d) 'resource (stream)' — corrected 2026-09-21,
///     when the §2.7 folds probed both spellings)
///   readdir($d) then answers '.', so the program runs on
/// ```
///
/// A cross-kind closer **returns without closing**. What keeps that out of this
/// rung is not the shape of `proc_open` but [`CLOSERS`]: the closing table is
/// keyed by handle kind, `fclose`/`gzclose`/`bzclose` do not list `dir`, and
/// [`site_verdict`] answers `Keep` there — so no `Closed` is ever claimed and
/// the statement after it is judged against an open handle. Measured on the
/// binary: `$d = opendir('.'); fclose($d); fdatasync($d);` is silent, and
/// `fdatasync` is a **rowed** position (ADR-0097 §2.5) with no kind of its own,
/// which is exactly the shape a future `readdir` row would have. So the
/// dependency is the kind table, not the absence of dir-handle rows in
/// `docs/research/phpsrc-mining/resource_params.toml` — and it is already
/// pinned, by `fclose_does_not_close_a_directory_handle` in
/// `tests/it/resource_values.rs`, which fails loudly the day `dir` joins one of
/// those three closers' kind lists.
///
/// The row absence is a second, independent shield, and it is the weaker one: it
/// makes the whole dir family unjudgeable rather than judged-correctly. It is
/// pinned beside the rest, in `tests/it/resource_producers.rs`
/// (`no_dir_handle_consumer_carries_a_resource_row`), so that the next person to
/// mine one arrives at this paragraph rather than at the bug.
///
/// What the argument does **not** cover, named rather than implied: a produced
/// place handed to a position that wants a non-resource. That window is
/// **closed, not merely gated** — the place carrier is a heap carrier and seeds
/// no value-domain fact, because no `Val` is a resource (ADR-0035/0038), so the
/// ordinary argument relation finds nothing at `$pipes[0]` and says nothing.
/// `strlen($pipes[0])` is silent, pinned by
/// `a_produced_place_speaks_only_at_a_resource_position`. The resource position
/// (ADR-0097 §2.5) is the only seam that reads a place, and the only seam that
/// could report one.
///
/// [`CLOSERS`]: crate::resource::closers
/// [`site_verdict`]: crate::resource::closers
/// [`stmt_out_param_seeds`]: crate::out_params::stmt_out_param_seeds
/// [`WrittenWhen::ReturnTruthy`]: steins_catalog::WrittenWhen::ReturnTruthy
pub(crate) fn stmt_produced_places(
    w: &WalkCx,
    folder: &mut dyn Folder,
    kind: &StmtKind,
    env: &HashMap<String, Known>,
) -> Vec<(String, Vec<(VKey, HeapRes)>)> {
    let call = match kind {
        // Both spellings seed, and the bare statement is not the rarer one:
        // `proc_open($cmd, $spec, $pipes);` is what code that only wants the
        // pipes writes, and it binds exactly what the assignment form does.
        StmtKind::Call(call) => call,
        StmtKind::Assign { call: Some(call), .. } => call,
        _ => return Vec::new(),
    };
    produced_places(w, folder, call, env).into_iter().collect()
}

/// The places a guard call fills, on the branch polarity that proves it wrote
/// (ADR-0077 §3.1). `if (proc_open($cmd, $spec, $pipes)) { … }` is the one
/// spelling that reaches here — the shapes that assign the return inside the
/// condition lower to an opaque guard and are collected by nothing.
pub(crate) fn seed_produced_places(
    w: &WalkCx,
    folder: &mut dyn Folder,
    cond: &CondExpr,
    then: bool,
    env: &HashMap<String, Known>,
    store: &mut Store,
) {
    let mut calls = Vec::new();
    collect_truthy_calls(cond, then, &mut calls);
    let seeds: Vec<_> =
        calls.into_iter().filter_map(|call| produced_places(w, folder, call, env)).collect();
    apply_produced_places(w, seeds, store);
}

/// Bind what [`stmt_produced_places`] computed, after the statement's by-ref
/// invalidation forgot the same name — which is also what dropped the element
/// places the previous binding had (ADR-0098 §2.3), so nothing here has to
/// sweep.
pub(crate) fn apply_produced_places(
    w: &WalkCx,
    seeds: Vec<(String, Vec<(VKey, HeapRes)>)>,
    store: &mut Store,
) {
    for (var, places) in seeds {
        bind_produced_places(w, &var, places, store);
    }
}

/// One call's out-parameter places, or `None` where nothing is proven.
///
/// Every leg refuses **whole and silently**, and the legs are the out-parameter
/// seed's own (ADR-0077 §3.2/§3.6) plus the producer row's:
///
/// * a poisoned scope (ADR-0046) cannot say which frame a name is in;
/// * the callee must denote the **global** builtin, positionally
///   ([`out_param_seed_callee`]);
/// * the catalog must both row the position and state a written-when witness
///   for it — nothing is inferred from the row's mere existence;
/// * the call must supply the argument (the arity leg), and it must be a plain
///   local variable (the aliasing leg): `$this->pipes` and `$bag['p']` refuse,
///   because a place under them is the carrier ADR-0098 §3 holds back;
/// * the spec must be proven, which [`proc_open_places`] owns.
///
/// **The two catalog legs are unobservable while this rung has one row, and
/// they stay anyway.** [`proc_open_places`] answers `None` for every name but
/// `proc_open`, so deleting either the `out_params` leg or the written-when leg
/// changes no finding this test suite can construct — there is no second name to
/// reach them with. They are not pinned for that reason, and no test below
/// implies otherwise: `proc_open_is_rowed_at_position_two_with_a_return_truthy_witness`
/// pins the catalog's answer, never that this walk asked. They are kept because
/// the rung is written to take a second producer, and the day one arrives the
/// legs are what stops it from being seeded off a row that states no witness.
///
/// # How a produced place differs from a literal's once the walk moves on
///
/// [`bind_produced_places`] and `bind_handle_elements` make the same kind of
/// carrier, but the **base** they hang under is not in the same state, and that
/// shows one statement later. `$bag = [$h]` and `$pair = stream_socket_pair(…)`
/// both bind the base itself — a shape fact, a declared arm lane — while
/// `proc_open`'s `$pipes` is an out-parameter: the walk binds places under the
/// name and never binds the name. So at ADR-0070's by-value survival leg
/// ([`is_value_semantic`]), `$pipes` reads as a name with no lane to save, the
/// statement's conservative drop stands, and `unbind` takes the places with it
/// (ADR-0098 §2.3).
///
/// Measured, all three carrying a proven `Closed` into the next statement:
///
/// ```text
///                              $bag[0]   $pair[0]   $pipes[0]
///   nothing in between          reports   reports    reports
///   count($base);               reports   reports    silent
///   helper($base);              reports   reports    silent
///   helper($base[0]);           reports   reports    silent
///   $copy = $base;              reports   reports    reports
/// ```
///
/// It is not the project/builtin split and it is not the producer: it is whether
/// the **base** was ever bound. Every cell of the bottom row errs toward
/// silence, so there is no false positive in it; it is a missed finding, pinned
/// by `a_produced_place_under_an_unbound_base_does_not_survive_a_call` so that
/// the asymmetry is a decision rather than a surprise. Closing it is one clause
/// in [`is_value_semantic`] — a name with element places has a lane to save —
/// and that is a change to the ADR-0070 rung, not to this one.
///
/// [`is_value_semantic`]: crate::walk
fn produced_places(
    w: &WalkCx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    env: &HashMap<String, Known>,
) -> Option<(String, Vec<(VKey, HeapRes)>)> {
    // The poison gate (ADR-0046), the same one `out_param_seed` opens with: an
    // `extract()` or a variable-variable can rewrite the frame the places would
    // land in, so nothing here may name one.
    //
    // **Defence in depth, and measured to be exactly that.** Removing this leg,
    // the `!w.scope.poisoned` leg on the `stream_socket_pair` rung in
    // `assign.rs`, and the scope bit handed to `proc_open_places` below — all
    // three at once — changes no finding: `resource_call_effects` refuses a
    // poisoned scope before it records any closing call, and a place nothing
    // closed convicts nothing. So no test claims these gates are pinned;
    // `a_poisoned_scope_convicts_through_no_place` pins the posture they
    // protect, and says so.
    if w.scope.poisoned {
        return None;
    }
    let name = out_param_seed_callee(w.cx, call)?;
    if !steins_catalog::out_params(name)?.contains(&PROC_OPEN_PIPES)
        || steins_catalog::out_param_written_when(name, PROC_OPEN_PIPES).is_none()
    {
        return None;
    }
    let ArgValue::Var(var) = &call.args.get(PROC_OPEN_PIPES)?.value else { return None };
    let spec = &call.args.get(PROC_OPEN_SPEC)?.value;
    let places = proc_open_places(w.cx, folder, name, spec, env, w.scope.poisoned)?;
    // `store` is deliberately not a parameter: every leg above reads the call,
    // the catalog or the env, and the one reader that would want the store —
    // the spec held in a variable — goes through `Cx::resolve_literal`, which
    // takes the env lane alone.
    Some((var.clone(), places))
}

/// The 0-based position of `proc_open`'s `array $descriptor_spec` — the argument
/// whose proven shape decides the whole key set (ADR-0098 §2.2).
const PROC_OPEN_SPEC: usize = 1;
