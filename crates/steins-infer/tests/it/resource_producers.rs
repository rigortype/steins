//! ADR-0098 §4 slice 2 — the producers that hand back an **array** of handles.
//!
//! Slice 1 gave the store a place key and one binding site: an array literal
//! over a handle a variable already held. These two builtins are the other
//! direction — the array and every handle in it arrive together, from a call,
//! with no variable in between — so `$pipes[0]` is a resource from the moment it
//! exists rather than only when a literal put it there.
//!
//! Every runtime claim below was probed at 8.5.10 (the catalog pin) and the
//! transcripts live on `socket_pair_places` and `proc_open_places`. What is
//! worth pinning is not "does `proc_open` fill `$pipes`" but each place a
//! produced place could claim more than was proven:
//!
//! * the **key set is the spec's**, not the arity's — a `file` descriptor
//!   contributes no key and a sparse spec keeps its own numbers;
//! * an **unproven spec binds nothing**, whole rather than in part: the entry
//!   the walk cannot read may itself be a pipe, so the keys that *are* readable
//!   are not the key set;
//! * the two producers mint **distinct** allocations — closing `$pair[0]` must
//!   leave `$pair[1]` alone, which one shared id would get wrong;
//! * the §2.3 **sweep** still owns the places: a rebound base, a dynamic key and
//!   a second call into the same variable each leave nothing behind;
//! * each producer keeps its own **tripwire** — `stream_socket_pair`'s declared
//!   `array|false` and `proc_open`'s by-reference `$pipes` — so an engine that
//!   migrates either one switches the row off by speaking.

use std::collections::HashMap;

use steins_infer::{Diagnostic, EngineFolder, FoldEngine, Folder, ID, check_with};
use steins_sidecar::{
    BuiltinParam, ClassReflection, ConstantDefined, EnvInfo, FoldArg, FoldResult, PregCompile,
    Reflection,
};
use steins_syntax::SourceTree;

// ---------------------------------------------------------------------------
// Mock engine: the signatures and return declarations read off 8.5.10
// ---------------------------------------------------------------------------

fn p(name: &str, ty: Option<&str>) -> BuiltinParam {
    BuiltinParam {
        name: name.to_owned(),
        ty: ty.map(ToOwned::to_owned),
        by_ref: false,
        variadic: false,
        optional: false,
    }
}

fn optional(name: &str, ty: Option<&str>) -> BuiltinParam {
    BuiltinParam { optional: true, ..p(name, ty) }
}

fn by_ref(name: &str) -> BuiltinParam {
    BuiltinParam { by_ref: true, ..p(name, None) }
}

/// An engine on the catalog pin, answering exactly what `ReflectionFunction`
/// answers at 8.5.10 for the names these tests call.
struct Engine {
    params: HashMap<String, Vec<BuiltinParam>>,
    returns: HashMap<String, String>,
    absent: Vec<String>,
    version: String,
}

impl Engine {
    fn pinned() -> Self {
        let mut params = HashMap::new();
        // `stream_socket_pair(int $domain, int $type, int $protocol): array|false`
        params.insert(
            "stream_socket_pair".to_owned(),
            vec![p("domain", Some("int")), p("type", Some("int")), p("protocol", Some("int"))],
        );
        // `proc_open($command, array $descriptor_spec, &$pipes, …)` — no
        // declared return type, which is what keeps it a resource producer row.
        params.insert(
            "proc_open".to_owned(),
            vec![
                p("command", Some("array|string")),
                p("descriptor_spec", Some("array")),
                by_ref("pipes"),
                optional("cwd", Some("?string")),
                optional("env_vars", Some("?array")),
                optional("options", Some("?array")),
            ],
        );
        params.insert("fclose".to_owned(), vec![p("stream", None)]);
        params.insert("fread".to_owned(), vec![p("stream", None), p("length", Some("int"))]);
        params.insert("fwrite".to_owned(), vec![p("stream", None), p("data", Some("string"))]);
        params.insert("feof".to_owned(), vec![p("stream", None)]);
        params.insert("proc_close".to_owned(), vec![p("process", None)]);
        params.insert(
            "stream_get_contents".to_owned(),
            vec![p("stream", None), optional("length", Some("?int"))],
        );
        params.insert(
            "fopen".to_owned(),
            vec![
                p("filename", Some("string")),
                p("mode", Some("string")),
                optional("use_include_path", Some("bool")),
                optional("context", None),
            ],
        );
        params.insert("strlen".to_owned(), vec![p("string", Some("string"))]);
        let mut returns = HashMap::new();
        returns.insert("stream_socket_pair".to_owned(), "array|false".to_owned());
        returns.insert("strlen".to_owned(), "int".to_owned());
        Engine { params, returns, absent: Vec::new(), version: "8.5.10".to_owned() }
    }

    /// The migration tripwire, in the shape a producer whose return type PHP
    /// CAN spell must take: the declaration itself changing.
    fn returning(mut self, name: &str, ty: &str) -> Self {
        self.returns.insert(name.to_owned(), ty.to_owned());
        self
    }

    fn by_value_at(mut self, name: &str, index: usize) -> Self {
        self.params.get_mut(name).expect("a known name")[index].by_ref = false;
        self
    }

    fn without(mut self, name: &str) -> Self {
        self.absent.push(name.to_owned());
        self
    }

    fn on_php(mut self, version: &str) -> Self {
        self.version = version.to_owned();
        self
    }
}

impl FoldEngine for Engine {
    fn env(&mut self) -> Option<EnvInfo> {
        Some(EnvInfo {
            php_version: self.version.clone(),
            extensions: vec!["Core".to_owned(), "standard".to_owned()],
            sapi: "cli".to_owned(),
            int_size: Some(8),
        })
    }
    fn reflect(&mut self, target: &str) -> Option<Reflection> {
        let key = target.to_ascii_lowercase();
        let known = self.params.contains_key(&key) && !self.absent.contains(&key);
        Some(Reflection {
            target: target.to_owned(),
            function_exists: known,
            class_like_exists: false,
            return_type: known.then(|| self.returns.get(&key).cloned()).flatten(),
            return_type_tentative: false,
            params_total: None,
            params_required: None,
            params: known.then(|| self.params[&key].clone()),
        })
    }
    fn reflect_class(&mut self, _target: &str) -> Option<ClassReflection> {
        None
    }
    fn fold(&mut self, _name: &str, _args: &[FoldArg], _strict: bool) -> FoldResult {
        FoldResult::widen("stub")
    }
    fn preg_compile(&mut self, _pattern: &str) -> Option<PregCompile> {
        None
    }
    fn constant_defined(&mut self, _name: &str) -> Option<ConstantDefined> {
        None
    }
    fn restarts(&self) -> u32 {
        0
    }
}

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

fn findings_with(src: &str, folder: &mut dyn Folder) -> Vec<Diagnostic> {
    let tree = SourceTree::parse(src);
    check_with(&tree, &[], "t.php", folder)
}

fn mismatches_on(src: &str, engine: Engine) -> Vec<String> {
    let mut folder = EngineFolder::with_engine(engine);
    findings_with(src, &mut folder).into_iter().filter(|d| d.id == ID).map(|d| d.message).collect()
}

fn coercive(body: &str) -> String {
    format!("<?php\n{body}")
}

fn strict(body: &str) -> String {
    format!("<?php\ndeclare(strict_types=1);\n{body}")
}

/// One finding, identical in both coercion modes — a resource verdict never
/// depends on the mode (ADR-0097 §1.1), so every pin here asserts both.
fn one_in_both_modes(body: &str, expected: &str) {
    for src in [coercive(body), strict(body)] {
        let out = mismatches_on(&src, Engine::pinned());
        assert_eq!(out, vec![expected.to_owned()], "for:\n{src}");
    }
}

fn silent_in_both_modes(body: &str) {
    silent_in_both_modes_on(body, Engine::pinned);
}

fn silent_in_both_modes_on(body: &str, engine: fn() -> Engine) {
    for src in [coercive(body), strict(body)] {
        let out = mismatches_on(&src, engine());
        assert!(out.is_empty(), "expected silence, got {out:?} for:\n{src}");
    }
}

/// The `=== false` guard `stream_socket_pair`'s failure arm asks for, spelled
/// the way the manual spells it.
const PAIR: &str =
    "$pair = stream_socket_pair(STREAM_PF_UNIX, STREAM_SOCK_STREAM, 0);\n\
     if ($pair === false) { throw new \\RuntimeException('x'); }\n";

/// A `proc_open` whose spec the walk can read whole: `0` a readable pipe, `2` a
/// writable one, and no other key.
const PIPES: &str =
    "$proc = proc_open('true', [0 => ['pipe', 'r'], 2 => ['pipe', 'w']], $pipes);\n";

// ---------------------------------------------------------------------------
// `stream_socket_pair`: exactly two places, both open, both distinct
// ---------------------------------------------------------------------------

#[test]
fn both_halves_of_a_pair_are_open_stream_handles() {
    // Probed at 8.5.10: `array_keys($p) === [0, 1]`, both `resource (stream)`,
    // both open. So every stream position accepts either, and nothing reports.
    silent_in_both_modes(&format!(
        "{PAIR}fwrite($pair[0], 'x');\nfread($pair[1], 1);\nfclose($pair[0]);\nfclose($pair[1]);\n"
    ));
}

#[test]
fn closing_one_half_convicts_a_later_use_of_that_half() {
    one_in_both_modes(
        &format!("{PAIR}fclose($pair[0]);\nfread($pair[0], 1);\n"),
        "argument $pair[0] to fread() cannot become resource $stream — the handle is closed; proven TypeError (the position needs an open handle, in either mode)",
    );
}

#[test]
fn the_two_halves_are_distinct_allocations() {
    // The measurement that forces two ids: `get_resource_id()` answered 4 and 5
    // for one pair, and after `fclose($p[0])` the probe read
    // `is_resource($p[0]) === false` beside `is_resource($p[1]) === true`. One
    // shared id would close both here and manufacture a finding on `$pair[1]`.
    silent_in_both_modes(&format!("{PAIR}fclose($pair[0]);\nfread($pair[1], 1);\n"));
}

#[test]
fn only_two_places_exist() {
    // There is no `$pair[2]`: the producer binds keys `0` and `1` and nothing
    // else, so a third key is an ordinary unbound offset and silent both ways.
    silent_in_both_modes(&format!("{PAIR}fclose($pair[2]);\nfread($pair[2], 1);\n"));
}

#[test]
fn a_rebound_pair_takes_its_places_with_it() {
    // ADR-0098 §2.3's sweep, on a producer's places: `$pair` is a fresh array by
    // the time the read happens and nothing is proven about `$pair[0]`.
    silent_in_both_modes(&format!(
        "{PAIR}fclose($pair[0]);\n$pair = [];\nfread($pair[0], 1);\n"
    ));
}

#[test]
fn a_dynamic_key_names_no_half() {
    // §3: no place, no binding, silence — the walk cannot say which half `$i`
    // selected, so the close lands nowhere and neither read convicts.
    silent_in_both_modes(&format!(
        "$i = 0;\n{PAIR}fclose($pair[$i]);\nfread($pair[0], 1);\nfread($pair[1], 1);\n"
    ));
}

#[test]
fn a_branch_that_may_not_have_closed_a_half_convicts_nothing() {
    silent_in_both_modes(&format!(
        "{PAIR}if (random_int(0, 1) === 1) {{ fclose($pair[0]); }}\nfread($pair[0], 1);\n"
    ));
}

#[test]
fn a_second_pair_replaces_the_first_ones_places() {
    // The rebind sweep again, through the producer rather than a literal: the
    // close landed on the FIRST pair's handle, and `$pair[0]` now names the
    // second pair's open one.
    silent_in_both_modes(&format!("{PAIR}fclose($pair[0]);\n{PAIR}fread($pair[0], 1);\n"));
}

// ---------------------------------------------------------------------------
// `stream_socket_pair`: the tripwire and the gate
// ---------------------------------------------------------------------------

#[test]
fn an_engine_that_declares_a_different_return_disowns_the_row() {
    // The migration this tripwire is for: a PHP that hands back sockets as
    // objects, or as anything but `array|false`. No such PHP exists today —
    // tested anyway, since the tripwire's whole job is to be right on the day
    // one does. Curation yields to the engine (ADR-0056 §1).
    for declared in ["Socket[]|false", "array", "iterable|false", "list<resource>|false"] {
        let src = coercive(&format!("{PAIR}fclose($pair[0]);\nfread($pair[0], 1);\n"));
        let out = mismatches_on(&src, Engine::pinned().returning("stream_socket_pair", declared));
        assert!(out.is_empty(), "`{declared}` must disown the row; got {out:?}");
    }
}

#[test]
fn a_name_the_engine_does_not_have_binds_nothing() {
    silent_in_both_modes_on(
        &format!("{PAIR}fclose($pair[0]);\nfread($pair[0], 1);\n"),
        || Engine::pinned().without("stream_socket_pair"),
    );
}

#[test]
fn a_project_function_of_the_same_name_answers_instead() {
    // A project shadow is the better answer, and it is not this producer.
    silent_in_both_modes(&format!(
        "function stream_socket_pair(int $d, int $t, int $p): array {{ return []; }}\n\
         {PAIR}fclose($pair[0]);\nfread($pair[0], 1);\n"
    ));
}

// ---------------------------------------------------------------------------
// `proc_open`: one place per `pipe` descriptor, at the spec's own key
// ---------------------------------------------------------------------------

#[test]
fn a_pipe_descriptor_key_is_a_place() {
    // Probed at 8.5.10: `[0 => ['pipe','r'], 2 => ['pipe','w']]` yields keys
    // `[0, 2]`, both `resource (stream)`, and `fclose($pipes[2])` leaves the
    // element holding `resource (closed)`.
    one_in_both_modes(
        &format!("{PIPES}fclose($pipes[2]);\nfread($pipes[2], 1);\n"),
        "argument $pipes[2] to fread() cannot become resource $stream — the handle is closed; proven TypeError (the position needs an open handle, in either mode)",
    );
}

#[test]
fn an_open_pipe_is_what_a_stream_position_asks_for() {
    silent_in_both_modes(&format!(
        "{PIPES}fwrite($pipes[0], 'x');\nstream_get_contents($pipes[2]);\n\
         fclose($pipes[0]);\nfclose($pipes[2]);\nproc_close($proc);\n"
    ));
}

#[test]
fn the_pipes_are_streams_and_the_return_is_the_process() {
    // Two kinds from one call (ADR-0097 §2.1): `proc_close` closes the `process`
    // the call returned, and `$pipes[0]` is a `stream` beside it. Closing the
    // process is not closing a pipe.
    silent_in_both_modes(&format!("{PIPES}proc_close($proc);\nfread($pipes[0], 1);\n"));
}

#[test]
fn a_key_the_spec_does_not_pipe_is_no_place() {
    // Probed: `[0 => ['file','/dev/null','r'], 1 => ['pipe','w']]` yields the
    // single key `1` — a `file` descriptor produces NO entry. So `$pipes[0]`
    // here names nothing, and a close of it convicts nothing later.
    silent_in_both_modes(
        "$proc = proc_open('true', [0 => ['file', '/dev/null', 'r'], 1 => ['pipe', 'w']], $pipes);\n\
         fclose($pipes[0]);\nfread($pipes[0], 1);\n",
    );
    // …while the key the same spec DOES pipe is a place, in the same file.
    one_in_both_modes(
        "$proc = proc_open('true', [0 => ['file', '/dev/null', 'r'], 1 => ['pipe', 'w']], $pipes);\n\
         fclose($pipes[1]);\nfread($pipes[1], 1);\n",
        "argument $pipes[1] to fread() cannot become resource $stream — the handle is closed; proven TypeError (the position needs an open handle, in either mode)",
    );
}

#[test]
fn the_other_descriptor_words_produce_no_entry_either() {
    // Probed at 8.5.10, each yielding an empty `$pipes` or one without the key:
    // `['null']`, `['redirect', 1]`, and a stream resource used as a descriptor.
    for spec in ["[1 => ['null']]", "[1 => ['redirect', 1]]"] {
        silent_in_both_modes(&format!(
            "$proc = proc_open('true', {spec}, $pipes);\nfclose($pipes[1]);\nfread($pipes[1], 1);\n"
        ));
    }
}

#[test]
fn socket_and_pty_descriptors_stay_out_although_they_do_produce_an_entry() {
    // The deliberate under-approximation on `proc_open_places`: both words were
    // probed to yield an entry at 8.5.10 (`[1 => ['socket']]` → key `1`;
    // `[0 => ['pty'], 1 => ['pty']]` → keys `0` and `1`), and neither is
    // admitted. A missed finding, which is where the family already was.
    for spec in ["[1 => ['socket']]", "[0 => ['pty'], 1 => ['pty']]"] {
        silent_in_both_modes(&format!(
            "$proc = proc_open('true', {spec}, $pipes);\nfclose($pipes[1]);\nfread($pipes[1], 1);\n"
        ));
    }
}

#[test]
fn a_socket_descriptor_leaves_the_rest_of_the_spec_readable_and_a_pty_does_not() {
    // The two words are held back DIFFERENTLY, because PHP treats them
    // differently. `socket` is accepted on any build — probed at 8.5.10,
    // `[1 => ['socket'], 0 => ['pipe','r']]` returns a process and fills keys
    // `[1, 0]` — so the spec stays readable and only the socket's own key goes
    // unclaimed.
    one_in_both_modes(
        "$proc = proc_open('true', [1 => ['socket'], 0 => ['pipe', 'r']], $pipes);\n\
         fclose($pipes[0]);\nfread($pipes[0], 1);\n",
        "argument $pipes[0] to fread() cannot become resource $stream — the handle is closed; proven TypeError (the position needs an open handle, in either mode)",
    );
    // `pty` needs a build whose `proc_open` has pseudo-terminal support. This
    // one has it, but a build without it cannot both refuse the word and write
    // the other keys — so the whole spec is refused rather than the word alone,
    // and `$pipes[0]` beside a `pty` is not a place.
    silent_in_both_modes(
        "$proc = proc_open('true', [1 => ['pty'], 0 => ['pipe', 'r']], $pipes);\n\
         fclose($pipes[0]);\nfread($pipes[0], 1);\n",
    );
}

// ---------------------------------------------------------------------------
// `proc_open`: the specs on which PHP writes NOTHING, every time it runs
// ---------------------------------------------------------------------------

#[test]
fn the_descriptor_word_is_compared_byte_for_byte() {
    // php-src compares the word case-SENSITIVELY. Probed at 8.5.10, with a
    // sentinel in `$pipes` before each call:
    //
    //   [0 => ['PIPE','r']]  → Warning: proc_open(): PIPE is not a valid
    //                          descriptor spec/mode;  ret false;  $pipes untouched
    //   [0 => ['Pipe','r']]  → the same, naming `Pipe`
    //   [0 => ['pipe','r']]  → ret resource;  array_keys($pipes) === [0]
    //
    // The failure is not a path: it is every execution of that line. So a place
    // minted for `PIPE` would convict a statement PHP never reaches.
    for word in ["PIPE", "Pipe", "pIpE"] {
        silent_in_both_modes(&format!(
            "$proc = proc_open('true', [0 => ['{word}', 'r']], $pipes);\n\
             fclose($pipes[0]);\nfread($pipes[0], 1);\n"
        ));
    }
    // The other direction, so the silence above is the comparison and not a
    // spec this walk stopped reading: the lowercase spelling still convicts.
    one_in_both_modes(
        "$proc = proc_open('true', [0 => ['pipe', 'r']], $pipes);\n\
         fclose($pipes[0]);\nfread($pipes[0], 1);\n",
        "argument $pipes[0] to fread() cannot become resource $stream — the handle is closed; proven TypeError (the position needs an open handle, in either mode)",
    );
    // The same case-sensitivity, on the words that produce no entry: probed,
    // `FILE`, `NULL` and `REDIRECT` each warn `… is not a valid descriptor
    // spec/mode` and leave `$pipes` untouched, so the `pipe` beside them is not
    // a place either.
    for spec in [
        "[0 => ['FILE', '/dev/null', 'r'], 1 => ['pipe', 'w']]",
        "[0 => ['NULL'], 1 => ['pipe', 'w']]",
        "[1 => ['pipe', 'w'], 2 => ['REDIRECT', 1]]",
    ] {
        silent_in_both_modes(&format!(
            "$proc = proc_open('true', {spec}, $pipes);\n\
             fclose($pipes[1]);\nfread($pipes[1], 1);\n"
        ));
    }
}

#[test]
fn an_unaccepted_descriptor_word_refuses_the_whole_spec() {
    // Whole, not the one entry: PHP wrote nothing at all, so the readable pipe
    // beside it describes no handle either.
    silent_in_both_modes(
        "$proc = proc_open('true', [0 => ['PIPE', 'r'], 1 => ['pipe', 'w']], $pipes);\n\
         fclose($pipes[1]);\nfread($pipes[1], 1);\n",
    );
    silent_in_both_modes(
        "$proc = proc_open('true', [0 => ['nonsense'], 1 => ['pipe', 'w']], $pipes);\n\
         fclose($pipes[1]);\nfread($pipes[1], 1);\n",
    );
}

#[test]
fn a_negative_key_refuses_the_whole_spec() {
    // Probed at 8.5.10, and for every descriptor word alike (`pipe`, `null`,
    // `file`, `socket`, `pty`): `[-1 => …]` warns `proc_open(): Unable to copy
    // file descriptor 5 (for pipe) into file descriptor -1: Bad file
    // descriptor`, answers `false`, and leaves `$pipes` untouched.
    silent_in_both_modes(
        "$proc = proc_open('true', [-1 => ['pipe', 'r']], $pipes);\n\
         fclose($pipes[-1]);\nfread($pipes[-1], 1);\n",
    );
    silent_in_both_modes(
        "$proc = proc_open('true', [-1 => ['pipe', 'r'], 0 => ['pipe', 'r']], $pipes);\n\
         fclose($pipes[0]);\nfread($pipes[0], 1);\n",
    );
    // …while a large NON-negative key is an ordinary place: probed,
    // `[100 => ['pipe','r']]` yields `array_keys($pipes) === [100]`.
    one_in_both_modes(
        "$proc = proc_open('true', [100 => ['pipe', 'r']], $pipes);\n\
         fclose($pipes[100]);\nfread($pipes[100], 1);\n",
        "argument $pipes[100] to fread() cannot become resource $stream — the handle is closed; proven TypeError (the position needs an open handle, in either mode)",
    );
}

#[test]
fn a_descriptor_missing_a_cell_php_demands_refuses_the_whole_spec() {
    // Each of these raises a `ValueError` before `proc_open` returns anything,
    // so no statement after the call runs at all. Probed at 8.5.10, one message
    // per line:
    //
    //   [0 => ['pipe']]                 ValueError: Missing mode parameter for 'pipe'
    //   [0 => ['file']]                 ValueError: Missing file name parameter for 'file'
    //   [0 => ['file','/dev/null']]     ValueError: Missing mode parameter for 'file'
    //   [2 => ['redirect']]             ValueError: Missing redirection target
    for spec in [
        "[0 => ['pipe']]",
        "[0 => ['pipe'], 1 => ['pipe', 'w']]",
        "[0 => ['file'], 1 => ['pipe', 'w']]",
        "[0 => ['file', '/dev/null'], 1 => ['pipe', 'w']]",
        "[2 => ['redirect'], 1 => ['pipe', 'w']]",
    ] {
        silent_in_both_modes(&format!(
            "$proc = proc_open('true', {spec}, $pipes);\n\
             fclose($pipes[1]);\nfread($pipes[1], 1);\n\
             fclose($pipes[0]);\nfread($pipes[0], 1);\n"
        ));
    }
}

#[test]
fn only_the_presence_of_the_mode_cell_is_checked_and_never_its_value() {
    // The engine does not validate the mode either. Probed at 8.5.10, both of
    // these return a process and fill key `0`: `[0 => ['pipe','zzz']]` and
    // `[0 => ['pipe', 5]]`. So a place is bound for both, and a rule that read
    // the mode would refuse a spec PHP accepts.
    for cell in ["'zzz'", "5", "''"] {
        one_in_both_modes(
            &format!(
                "$proc = proc_open('true', [0 => ['pipe', {cell}]], $pipes);\n\
                 fclose($pipes[0]);\nfread($pipes[0], 1);\n"
            ),
            "argument $pipes[0] to fread() cannot become resource $stream — the handle is closed; proven TypeError (the position needs an open handle, in either mode)",
        );
    }
}

#[test]
fn a_string_key_in_the_spec_binds_nothing() {
    // Probed: `ValueError: proc_open(): Argument #2 ($descriptor_spec) must be
    // an integer indexed array`. The call raises before it returns, so the
    // `pipe` under the string key is not a place — and neither is the integer
    // key beside it.
    silent_in_both_modes(
        "$proc = proc_open('true', ['a' => ['pipe', 'r']], $pipes);\n\
         fclose($pipes['a']);\nfread($pipes['a'], 1);\n",
    );
    silent_in_both_modes(
        "$proc = proc_open('true', ['a' => ['pipe', 'r'], 0 => ['pipe', 'r']], $pipes);\n\
         fclose($pipes[0]);\nfread($pipes[0], 1);\n",
    );
}

#[test]
fn a_sparse_spec_keeps_its_own_numbers() {
    // Probed: `[0 => ['pipe','r'], 5 => ['pipe','w']]` yields keys `[0, 5]`.
    // The key is the descriptor's, never its ordinal.
    one_in_both_modes(
        "$proc = proc_open('true', [0 => ['pipe', 'r'], 5 => ['pipe', 'w']], $pipes);\n\
         fclose($pipes[5]);\nfread($pipes[5], 1);\n",
        "argument $pipes[5] to fread() cannot become resource $stream — the handle is closed; proven TypeError (the position needs an open handle, in either mode)",
    );
    silent_in_both_modes(
        "$proc = proc_open('true', [0 => ['pipe', 'r'], 5 => ['pipe', 'w']], $pipes);\n\
         fclose($pipes[5]);\nfread($pipes[1], 1);\n",
    );
}

#[test]
fn a_list_spelled_spec_gets_the_auto_indexes() {
    // Probed: `[['pipe','r'], ['pipe','w']]` yields keys `[0, 1]` — PHP's own
    // auto-index, which is what the walk's array normalization computes.
    one_in_both_modes(
        "$proc = proc_open('true', [['pipe', 'r'], ['pipe', 'w']], $pipes);\n\
         fclose($pipes[1]);\nfread($pipes[1], 1);\n",
        "argument $pipes[1] to fread() cannot become resource $stream — the handle is closed; proven TypeError (the position needs an open handle, in either mode)",
    );
}

#[test]
fn an_empty_spec_binds_nothing() {
    silent_in_both_modes("$proc = proc_open('true', [], $pipes);\nfclose($pipes[0]);\nfread($pipes[0], 1);\n");
}

#[test]
fn a_spec_held_in_a_variable_is_proven_the_same_way() {
    // The proof is the walk's, not the syntax's: a variable bound to a literal
    // array resolves to the same spec and binds the same keys.
    one_in_both_modes(
        "$spec = [1 => ['pipe', 'w']];\n\
         $proc = proc_open('true', $spec, $pipes);\nfclose($pipes[1]);\nfread($pipes[1], 1);\n",
        "argument $pipes[1] to fread() cannot become resource $stream — the handle is closed; proven TypeError (the position needs an open handle, in either mode)",
    );
}

// ---------------------------------------------------------------------------
// `proc_open`: the silences, pinned as hard as the findings
// ---------------------------------------------------------------------------

#[test]
fn an_unproven_spec_binds_nothing_at_all() {
    // ADR-0098 §2.2: an unproven spec binds nothing — silence, not a guess. The
    // parameter is an ordinary `array` the walk knows nothing about.
    silent_in_both_modes(
        "function f(array $spec): void {\n\
         $proc = proc_open('true', $spec, $pipes);\n\
         fclose($pipes[0]);\nfread($pipes[0], 1);\n}\n",
    );
}

#[test]
fn one_unreadable_descriptor_refuses_the_whole_spec() {
    // Refused WHOLE, not in part. The entry the walk cannot read may itself be
    // a pipe, so binding the readable ones would claim `$pipes` has exactly the
    // keys that were readable — a claim about the entries as a set, which is
    // the thing that was not proven. So key `0` stays unbound too.
    silent_in_both_modes(
        "function f(array $other): void {\n\
         $proc = proc_open('true', [0 => ['pipe', 'r'], 1 => $other], $pipes);\n\
         fclose($pipes[0]);\nfread($pipes[0], 1);\n}\n",
    );
}

#[test]
fn a_descriptor_whose_word_is_not_a_literal_refuses_the_whole_spec() {
    silent_in_both_modes(
        "function f(string $word): void {\n\
         $proc = proc_open('true', [0 => [$word, 'r'], 1 => ['pipe', 'w']], $pipes);\n\
         fclose($pipes[1]);\nfread($pipes[1], 1);\n}\n",
    );
}

#[test]
fn a_dynamic_key_names_no_pipe() {
    silent_in_both_modes(&format!(
        "$i = 0;\n{PIPES}fclose($pipes[$i]);\nfread($pipes[0], 1);\nfread($pipes[2], 1);\n"
    ));
}

#[test]
fn a_rebound_pipes_takes_its_places_with_it() {
    silent_in_both_modes(&format!("{PIPES}fclose($pipes[0]);\n$pipes = [];\nfread($pipes[0], 1);\n"));
}

#[test]
fn a_second_call_into_the_same_variable_replaces_its_places() {
    // The by-reference invalidation drops `$pipes` and its places (§2.3), and
    // the second call mints its own: the close landed on the FIRST call's
    // handle, and `$pipes[0]` now names the second's open one.
    silent_in_both_modes(&format!("{PIPES}fclose($pipes[0]);\n{PIPES}fread($pipes[0], 1);\n"));
}

#[test]
fn a_pipes_argument_that_is_not_a_plain_variable_binds_nothing() {
    // The aliasing leg (ADR-0077 §3.6): a property is the carrier ADR-0098 §3
    // holds back, so nothing under it is nameable.
    silent_in_both_modes(
        "class C { public array $pipes = []; public function f(): void {\n\
         $proc = proc_open('true', [0 => ['pipe', 'r']], $this->pipes);\n\
         fclose($this->pipes[0]);\nfread($this->pipes[0], 1);\n} }\n",
    );
}

#[test]
fn a_named_argument_defeats_the_positional_reading() {
    // `out_param_seed_callee`'s gate: a call whose positional mapping a named
    // argument defeated cannot say which argument is the spec.
    silent_in_both_modes(
        "$proc = proc_open('true', pipes: $pipes, descriptor_spec: [0 => ['pipe', 'r']]);\n\
         fclose($pipes[0]);\nfread($pipes[0], 1);\n",
    );
}

#[test]
fn a_branch_that_may_not_have_closed_a_pipe_convicts_nothing() {
    silent_in_both_modes(&format!(
        "{PIPES}if (random_int(0, 1) === 1) {{ fclose($pipes[0]); }}\nfread($pipes[0], 1);\n"
    ));
}

// ---------------------------------------------------------------------------
// `proc_open`: the gate, switched off one condition at a time
// ---------------------------------------------------------------------------

#[test]
fn an_engine_that_takes_pipes_by_value_disowns_the_row() {
    // The by-reference tripwire: a position the engine reports by value is one
    // this call cannot write, whatever the pinned stub said.
    silent_in_both_modes_on(
        &format!("{PIPES}fclose($pipes[0]);\nfread($pipes[0], 1);\n"),
        || Engine::pinned().by_value_at("proc_open", 2),
    );
}

#[test]
fn an_engine_that_declares_a_return_type_disowns_the_row() {
    // The ADR-0056 §8.2 tripwire `proc_open` already carries as a producer: a
    // declared return is a migration, and the pipes go with it.
    silent_in_both_modes_on(
        &format!("{PIPES}fclose($pipes[0]);\nfread($pipes[0], 1);\n"),
        || Engine::pinned().returning("proc_open", "ProcessHandle|false"),
    );
}

#[test]
fn a_php_off_the_catalog_pin_binds_nothing() {
    // The spec reading is a probe at 8.5.10 and says nothing about any other
    // minor (ADR-0056 §2), which is the gate `builtin_resource_return` applies.
    silent_in_both_modes_on(
        &format!("{PIPES}fclose($pipes[0]);\nfread($pipes[0], 1);\n"),
        || Engine::pinned().on_php("8.4.12"),
    );
}

#[test]
fn a_project_function_named_proc_open_answers_instead() {
    silent_in_both_modes(&format!(
        "function proc_open($c, array $s, &$p) {{ $p = []; return false; }}\n\
         {PIPES}fclose($pipes[0]);\nfread($pipes[0], 1);\n"
    ));
}

// ---------------------------------------------------------------------------
// The catalog row `proc_open` needed, and what else it turns on
// ---------------------------------------------------------------------------

#[test]
fn proc_open_is_rowed_at_position_two_with_a_return_truthy_witness() {
    assert_eq!(steins_catalog::out_params("proc_open"), Some(&[2][..]));
    assert_eq!(
        steins_catalog::out_param_written_when("proc_open", 2),
        Some(steins_catalog::WrittenWhen::ReturnTruthy),
        "probed: a `proc_open` that answers `false` leaves `$pipes` untouched",
    );
    // Not `CallReturns`, which would seed at a bare statement the caller proved
    // nothing about — the distinction the row exists to make.
    assert_ne!(
        steins_catalog::out_param_written_when("proc_open", 2),
        Some(steins_catalog::WrittenWhen::CallReturns),
    );
    // What the row turns on beside the places: every OTHER position of
    // `proc_open` is now certified by value, so a fact about the command or the
    // spec survives the call (ADR-0070 condition 2). The engine's own arginfo
    // says so — `isPassedByReference()` is `false` for all five.
    for position in [0, 1, 3, 4, 5] {
        assert_eq!(steins_catalog::by_value_arg("proc_open", position), Some(true));
    }
    assert_eq!(steins_catalog::by_value_arg("proc_open", 2), Some(false));
}

// ---------------------------------------------------------------------------
// The failure arm: where a produced place stops speaking, and where it does not
// ---------------------------------------------------------------------------

#[test]
fn the_false_branch_of_the_pair_guard_has_no_places_in_it() {
    // The failure path is the one where NO handle was produced, and it is the
    // one a produced place must not describe. Nothing new is needed for it:
    // `$pair === false` refines the base to a singleton, and that refinement
    // already `unbind`s it — which is ADR-0098 §2.3's sweep, so the whole
    // `pair[` family goes with the base. A stale `Open` here would be a claim
    // about a handle `stream_socket_pair` never handed back.
    silent_in_both_modes(
        "$pair = stream_socket_pair(STREAM_PF_UNIX, STREAM_SOCK_STREAM, 0);\n\
         if ($pair === false) { fclose($pair[0]); fread($pair[0], 1); }\n",
    );
    // The other side of the same guard keeps them, which is the whole point.
    one_in_both_modes(
        "$pair = stream_socket_pair(STREAM_PF_UNIX, STREAM_SOCK_STREAM, 0);\n\
         if ($pair !== false) { fclose($pair[0]); fread($pair[0], 1); }\n",
        "argument $pair[0] to fread() cannot become resource $stream — the handle is closed; proven TypeError (the position needs an open handle, in either mode)",
    );
}

#[test]
fn a_place_convicts_only_through_a_close_that_returned() {
    // No guard at all, and the finding still stands — ADR-0097 §2.4's premise,
    // which this family has rested on since `fopen`: a closing call's RETURN is
    // the whole premise. On the path where the producer answered `false` there
    // is no handle, `fclose()` of a non-resource is a `TypeError` (ADR-0097
    // §1.1), and the statement this reports never runs. So the line reported is
    // reachable only on the path where the close happened, which is the path
    // the place describes.
    one_in_both_modes(
        "$pair = stream_socket_pair(STREAM_PF_UNIX, STREAM_SOCK_STREAM, 0);\n\
         fclose($pair[0]);\nfread($pair[0], 1);\n",
        "argument $pair[0] to fread() cannot become resource $stream — the handle is closed; proven TypeError (the position needs an open handle, in either mode)",
    );
    // The same premise is the whole of `proc_open`'s statement-position claim,
    // whose witness is `ReturnTruthy` and whose guard — when the code writes one
    // — is on the RETURN and not on `$pipes`. So a branch that proved the return
    // false does not take the places with it, and this reports inside it. The
    // report is not observable for the reason above, and closing the gap needs a
    // carrier tying the place to the return binding rather than a smaller table.
    one_in_both_modes(
        &format!("{PIPES}if ($proc === false) {{ fclose($pipes[0]); fread($pipes[0], 1); }}\n"),
        "argument $pipes[0] to fread() cannot become resource $stream — the handle is closed; proven TypeError (the position needs an open handle, in either mode)",
    );
}

#[test]
fn a_produced_place_speaks_only_at_a_resource_position() {
    // The place carrier is a HEAP carrier: it binds an identity and a state, and
    // it seeds no value-domain fact, because no `Val` is a resource
    // (ADR-0035/0038). So the ordinary argument relation, which reads the value
    // lane, finds nothing at `$pipes[0]` and says nothing — the resource
    // position (ADR-0097 §2.5) is the only seam that reads a place.
    silent_in_both_modes(&format!("{PIPES}strlen($pipes[0]);\n"));
    silent_in_both_modes(&format!("{PAIR}strlen($pair[0]);\n"));
}

#[test]
fn the_row_does_not_change_the_statement_no_effect_verdict() {
    // `proc_open` is `io.process`, which is not a discardable colour, so
    // `builtin_does_nothing` already refused it before the out-param row
    // existed; the row only makes it refuse one leg earlier.
    assert_eq!(steins_catalog::effect_labels("proc_open"), Some(&["io.process"][..]));
    let src = coercive("proc_open('true', [0 => ['pipe', 'r']], $pipes);\n");
    let mut folder = EngineFolder::with_engine(Engine::pinned());
    let out: Vec<_> = findings_with(&src, &mut folder)
        .into_iter()
        .filter(|d| d.id == steins_infer::STATEMENT_NO_EFFECT_ID)
        .collect();
    assert!(out.is_empty(), "a `proc_open` statement has an effect: {out:?}");
}

// ---------------------------------------------------------------------------
// The two positions the row is read at, and the gates that shut both
// ---------------------------------------------------------------------------

/// The finding `fclose($pipes[0]); fread($pipes[0], 1);` yields, spelled once —
/// every guard-position pin below is the same two statements under a different
/// header.
const CLOSED_PIPE_0: &str =
    "argument $pipes[0] to fread() cannot become resource $stream — the handle is closed; proven TypeError (the position needs an open handle, in either mode)";

#[test]
fn the_guard_position_seeds_on_every_shape_that_proves_the_call_returned_truthy() {
    // `seed_produced_places` in `branch.rs`, which the PR calls the strongest
    // position the row has and which nothing else here exercises. The witness is
    // `ReturnTruthy` (probed: a `proc_open` that answers `false` leaves `$pipes`
    // untouched), so a branch that proved the call truthy has proved the write.
    let spec = "[0 => ['pipe', 'r']]";
    let body = "fclose($pipes[0]);\nfread($pipes[0], 1);\n";
    for header in [
        format!("if (proc_open('true', {spec}, $pipes)) {{\n{body}}}\n"),
        format!("while (proc_open('true', {spec}, $pipes)) {{\n{body}}}\n"),
        format!("if (proc_open('true', {spec}, $pipes) && random_int(0, 1)) {{\n{body}}}\n"),
    ] {
        one_in_both_modes(&header, CLOSED_PIPE_0);
    }
    // …and the polarities that proved the opposite seed nothing. `!proc_open(…)`
    // and `proc_open(…) === false` both reach their then-branch having proved
    // the call answered falsy, which is the path where `$pipes` was never
    // written — a place there would describe a handle that does not exist.
    for header in [
        format!("if (!proc_open('true', {spec}, $pipes)) {{\n{body}}}\n"),
        format!("if (proc_open('true', {spec}, $pipes) === false) {{\n{body}}}\n"),
    ] {
        silent_in_both_modes(&header);
    }
}

#[test]
fn a_bare_proc_open_statement_binds_the_same_places_as_the_assignment_form() {
    // `stmt_produced_places` takes `StmtKind::Call` as well as the assignment,
    // and the bare form is what code that only wants the pipes writes. Every
    // other pin here spells `$proc = proc_open(…)`, so without this one the
    // `StmtKind::Call` arm is unexercised.
    one_in_both_modes(
        "proc_open('true', [0 => ['pipe', 'r']], $pipes);\nfclose($pipes[0]);\nfread($pipes[0], 1);\n",
        CLOSED_PIPE_0,
    );
}

#[test]
fn a_poisoned_scope_convicts_through_no_place() {
    // ADR-0046: `extract()` and a variable-variable can rewrite the frame under
    // any name, so nothing in a poisoned scope may say which binding a place is.
    //
    // **What this pins, exactly.** It is the POSTURE, not either producer's own
    // poison gate. Measured by mutation: deleting `!w.scope.poisoned` from the
    // `stream_socket_pair` rung in `assign.rs`, deleting the `w.scope.poisoned`
    // leg `produced_places` opens with, and handing `proc_open_places` a `false`
    // in place of the scope's bit — all three at once — changes no finding this
    // suite can construct, because `resource_call_effects` refuses a poisoned
    // scope before any closing call is recorded, and a place that is never
    // closed convicts nothing. The three gates are defence in depth and are
    // deliberately not claimed to be pinned; what IS pinned is that a poisoned
    // scope stays silent through this whole rung, which is the property the
    // gates exist to protect.
    for poison in ["extract($a);\n", "$$n = 1;\n", ""] {
        let trailing = if poison.is_empty() { "extract($a);\n" } else { "" };
        silent_in_both_modes(&format!(
            "function f(array $a, string $n): void {{\n\
             {poison}\
             $proc = proc_open('true', [0 => ['pipe', 'r']], $pipes);\n\
             fclose($pipes[0]);\nfread($pipes[0], 1);\n{trailing}}}\n"
        ));
        silent_in_both_modes(&format!(
            "function f(array $a, string $n): void {{\n\
             {poison}\
             $pair = stream_socket_pair(STREAM_PF_UNIX, STREAM_SOCK_STREAM, 0);\n\
             if ($pair === false) {{ return; }}\n\
             fclose($pair[0]);\nfread($pair[0], 1);\n{trailing}}}\n"
        ));
    }
    // An ordinary unresolved call is NOT a poisoner, so the silence above is
    // ADR-0046's and not "a function body the walk gave up on".
    one_in_both_modes(
        "function f(array $a): void {\n\
         nope($a);\n\
         $proc = proc_open('true', [0 => ['pipe', 'r']], $pipes);\n\
         fclose($pipes[0]);\nfread($pipes[0], 1);\n}\n",
        CLOSED_PIPE_0,
    );
    // …and the same two scopes clean, so the silence above is the poison and
    // not the function body.
    one_in_both_modes(
        "function f(array $a): void {\n\
         $proc = proc_open('true', [0 => ['pipe', 'r']], $pipes);\n\
         fclose($pipes[0]);\nfread($pipes[0], 1);\n}\n",
        CLOSED_PIPE_0,
    );
    one_in_both_modes(
        "function f(array $a): void {\n\
         $pair = stream_socket_pair(STREAM_PF_UNIX, STREAM_SOCK_STREAM, 0);\n\
         if ($pair === false) { return; }\n\
         fclose($pair[0]);\nfread($pair[0], 1);\n}\n",
        "argument $pair[0] to fread() cannot become resource $stream — the handle is closed; proven TypeError (the position needs an open handle, in either mode)",
    );
}

// ---------------------------------------------------------------------------
// Two measured asymmetries, pinned so that they stay decisions
// ---------------------------------------------------------------------------

#[test]
fn a_produced_place_under_an_unbound_base_does_not_survive_a_call() {
    // `$bag = [$h]` and `$pair = stream_socket_pair(…)` bind the BASE as well as
    // the places; `proc_open`'s `$pipes` is an out-parameter, so the walk binds
    // places under a name it never binds. At ADR-0070's by-value survival leg
    // (`is_value_semantic`) that name reads as one with no lane to save, the
    // statement's conservative drop stands, and `unbind` takes the places with
    // it (ADR-0098 §2.3).
    //
    // Every cell here errs toward SILENCE, so nothing in it is a false
    // positive — it is a missed finding, and it is pinned so that the
    // asymmetry is a decision rather than a surprise. Closing it is one clause
    // in `is_value_semantic`, which is the ADR-0070 rung and not this one.
    //
    // The fixtures run inside a FUNCTION BODY on purpose. At top level a
    // project call forgets every handle's state, because it could rebind a
    // global (ADR-0097 §2.4, the `global`/`$GLOBALS` hole #758 closed) — which
    // would silence the `helper(…)` rows for a reason that has nothing to do
    // with the base's binding. In a function the locals are out of a callee's
    // reach, so the only thing left to vary is the asymmetry this pins.
    let in_fn = |body: String| {
        format!("function helper(mixed $x = null): void {{}}\nfunction run(): void {{\n{body}}}\n")
    };
    let bag = "$h = fopen('php://memory', 'r');\nif ($h === false) { return; }\n$bag = [$h];\n";
    for interposed in ["count($B);\n", "helper($B);\n", "helper($B[0]);\n"] {
        let pipes = interposed.replace("$B", "$pipes");
        silent_in_both_modes(&in_fn(format!("{PIPES}fclose($pipes[0]);\n{pipes}fread($pipes[0], 1);\n")));
        // The same statement over a bound base keeps the place, both ways.
        let pair = interposed.replace("$B", "$pair");
        one_in_both_modes(
            &in_fn(format!("{PAIR}fclose($pair[0]);\n{pair}fread($pair[0], 1);\n")),
            "argument $pair[0] to fread() cannot become resource $stream — the handle is closed; proven TypeError (the position needs an open handle, in either mode)",
        );
        let literal = interposed.replace("$B", "$bag");
        one_in_both_modes(
            &in_fn(format!("{bag}fclose($bag[0]);\n{literal}fread($bag[0], 1);\n")),
            "argument $bag[0] to fread() cannot become resource $stream — the handle is closed; proven TypeError (the position needs an open handle, in either mode)",
        );
    }
    // It is the base's binding and not the call: a plain copy is no call at all,
    // and `$pipes`' places survive it.
    one_in_both_modes(
        &format!("{PIPES}fclose($pipes[0]);\n$copy = $pipes;\nfread($pipes[0], 1);\n"),
        CLOSED_PIPE_0,
    );
}

#[test]
fn no_dir_handle_consumer_carries_a_resource_row() {
    // The tripwire for the deferral argued on `stmt_produced_places`. PHP has
    // exactly one closing call that RETURNS without closing — probed at 8.5.10,
    // `fclose($dirHandle)` warns `cannot close the provided stream, as it must
    // not be manually closed`, answers `false`, leaves the handle open, and
    // `readdir()` then succeeds — and the deferral's premise is that a closing
    // call either closes or does not return.
    //
    // What actually keeps that out of reach is the kind-aware closing table
    // (`fclose_does_not_close_a_directory_handle`, in `resource_values.rs`),
    // which is the pin that fails loudly if `dir` ever joins `fclose`'s kinds.
    // The row absence below is the second, weaker shield: it makes the whole
    // dir family unjudgeable rather than judged correctly. It is pinned here so
    // that mining one arrives at the paragraph on `stmt_produced_places` rather
    // than at a surprise.
    //
    // All three are `resource|null $dir_handle` in the stubs at the pin, which
    // is why `mine-resource-params` declines them (a union is judged by the
    // ordinary relation once the arms lower).
    for name in ["readdir", "rewinddir", "closedir"] {
        assert!(
            steins_catalog::resource_param(name, 0).is_none(),
            "`{name}` has gained a resource row — read the deferral paragraph on \
             `stmt_produced_places` before landing it",
        );
    }
    // And the row this whole family does rest on is still there, so the loop
    // above is an absence and not a broken accessor.
    assert!(steins_catalog::resource_param("fread", 0).is_some());
}
