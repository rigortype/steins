//! An engine method or constructor can run user code through its arguments, and
//! the call-site rule of ADR-0021's second 2026-10-01 amendment holds it in both
//! lanes (issue #858, ADR-0099 §4.2). `new \RuntimeException($o)` converts `$o`
//! through `__toString`, as `new \DateTime($o)` and `PDO::query($o)` do; the
//! call keeps its row and gains the gap `user-code-reach`, unless the argument is
//! shown not to hold an object or the file declares `strict_types=1`.

use steins_infer::{EffectSummary, effect_summary};
use steins_syntax::SourceTree;

fn summary(src: &str, symbol: &str) -> EffectSummary {
    let tree = SourceTree::parse(src);
    let functions = tree.functions().to_vec();
    let classes = tree.classes().to_vec();
    effect_summary(&tree, &functions, &classes)
        .into_iter()
        .find(|s| s.symbol == symbol)
        .unwrap_or_else(|| panic!("no summary for {symbol}"))
}

/// A file declaring a class whose `__toString` throws, a project exception, and
/// one that forwards its argument to the engine's constructor, then `f` with the
/// given signature and body, in coercive mode or under `strict_types=1`. The
/// forwarding exception takes the same signature and passes `$o`.
fn file(strict: bool, signature: &str, body: &str) -> String {
    let declare = if strict { "declare(strict_types=1);\n" } else { "" };
    format!(
        "<?php\n{declare}\
         final class Name {{ public function __toString(): string {{ throw new \\LogicException('x'); }} }}\n\
         class AppError extends \\RuntimeException {{}}\n\
         class Forwarding extends \\RuntimeException {{\n\
             public function __construct({signature}) {{ parent::__construct($o); }}\n\
         }}\n\
         function f({signature}): mixed {{ {body} }}\n"
    )
}

/// Whether `symbol` reaches user code through an operand, in the effect lane and
/// in the throw lane.
fn reaches(src: &str, symbol: &str) -> (bool, bool) {
    let s = summary(src, symbol);
    (s.gaps.contains(&"user-code-reach"), s.throws_gaps.contains(&"user-code-reach"))
}

const BOTH: (bool, bool) = (true, true);
const NEITHER: (bool, bool) = (false, false);

#[test]
fn a_throwable_constructor_coerces_its_message_in_both_lanes() {
    let call = "return new \\RuntimeException($o);";
    assert_eq!(reaches(&file(false, "Name $o", call), "f"), BOTH);
    assert_eq!(reaches(&file(false, "$o", call), "f"), BOTH);
    // A proven string is nothing to convert, and `strict_types=1` makes an object
    // there a `TypeError` the engine raises itself.
    let string = "return new \\RuntimeException($s);";
    assert_eq!(reaches(&file(false, "string $s", string), "f"), NEITHER);
    assert_eq!(reaches(&file(true, "Name $o", call), "f"), NEITHER);
    assert_eq!(reaches(&file(true, "$o", call), "f"), NEITHER);
}

#[test]
fn the_row_is_kept_beside_the_gap() {
    let s = summary(&file(false, "Name $o", "return new \\DateTime($o);"), "f");
    assert_eq!(s.gaps, ["user-code-reach"], "{s:?}");
    assert_eq!(s.labels, ["nondet.time"], "{s:?}");
    let s = summary(&file(true, "Name $o", "return new \\DateTime($o);"), "f");
    assert!(s.gaps.is_empty() && s.exhaustive, "{s:?}");
    assert_eq!(s.labels, ["nondet.time"], "{s:?}");
}

#[test]
fn only_the_message_of_a_throwable_constructor_converts() {
    // `$code` is an `int`: an object there is a `TypeError` in either mode.
    let call = "return new \\RuntimeException('m', $o);";
    assert_eq!(reaches(&file(false, "Name $o", call), "f"), NEITHER);
    // `$previous` is an internal class, which no user code can be passed as.
    let call = "return new \\RuntimeException('m', 0, $o);";
    assert_eq!(reaches(&file(false, "?\\Throwable $o", call), "f"), NEITHER);
}

#[test]
fn a_project_exception_reaches_its_engine_ancestors_constructor() {
    let call = "return new AppError($o);";
    assert_eq!(reaches(&file(false, "Name $o", call), "f"), BOTH);
    assert_eq!(reaches(&file(true, "Name $o", call), "f"), NEITHER);
}

#[test]
fn parent_construct_into_an_engine_throwable_is_held_to_the_rule() {
    assert_eq!(reaches(&file(false, "Name $o", ""), "Forwarding::__construct"), BOTH);
    assert_eq!(reaches(&file(false, "$o", ""), "Forwarding::__construct"), BOTH);
    assert_eq!(reaches(&file(false, "string $o", ""), "Forwarding::__construct"), NEITHER);
    assert_eq!(reaches(&file(true, "Name $o", ""), "Forwarding::__construct"), NEITHER);
}

#[test]
fn a_date_time_constructor_and_factory_coerce_the_string() {
    for call in [
        "return new \\DateTime($o);",
        "return new \\DateTimeImmutable($o);",
        "return \\DateTime::createFromFormat('Y', $o);",
    ] {
        assert!(reaches(&file(false, "Name $o", call), "f").0, "{call}");
        assert!(!reaches(&file(true, "Name $o", call), "f").0, "{call}");
    }
    // The `DateTimeZone` is an internal class, which no user code can be passed as.
    let call = "return new \\DateTime('now', $z);";
    assert_eq!(reaches(&file(false, "?\\DateTimeZone $z", call), "f"), NEITHER);
}

/// `parent::query(…)` in a subclass of `PDO`: the exact receiver a project
/// wrapper writes.
#[test]
fn a_pdo_wrapper_forwarding_to_parent_is_held_to_the_rule() {
    let wrapper = |strict: bool, signature: &str, call: &str| {
        let declare = if strict { "declare(strict_types=1);\n" } else { "" };
        format!(
            "<?php\n{declare}\
             final class Name {{ public function __toString(): string {{ return 'x'; }} }}\n\
             class Db extends \\PDO {{\n\
                 public function run({signature}): mixed {{ return parent::{call}; }}\n\
             }}\n"
        )
    };
    let s = summary(&wrapper(false, "Name $o", "query($o)"), "Db::run");
    assert_eq!(s.gaps, ["user-code-reach"], "{s:?}");
    assert_eq!(s.labels, ["io.db"], "the row is kept: {s:?}");
    assert_eq!(reaches(&wrapper(false, "string $s", "query($s)"), "Db::run"), NEITHER);
    assert_eq!(reaches(&wrapper(false, "string $s", "query('SELECT 1')"), "Db::run"), NEITHER);
    assert_eq!(reaches(&wrapper(true, "Name $o", "query($o)"), "Db::run"), NEITHER);
    assert!(reaches(&wrapper(false, "Name $o", "exec($o)"), "Db::run").0);
    assert!(!reaches(&wrapper(true, "Name $o", "exec($o)"), "Db::run").0);
    // `?int $fetchMode` names a fetch mode that can construct a class: never ruled out.
    assert!(reaches(&wrapper(true, "int $m", "query('SELECT 1', $m)"), "Db::run").0);
}

/// `(new PDO)->query($s)` names its class but records no operand shapes, so the
/// row reads blind: `query`'s `$fetchModeArgs` can name a class to autoload in
/// either mode, and `exec`'s only parameter is a coerced string.
#[test]
fn a_receiver_without_operand_shapes_reads_blind() {
    let call = "return (new \\PDO('sqlite::memory:'))->query($s);";
    assert!(reaches(&file(true, "string $s", call), "f").0);
    let call = "return (new \\PDO('sqlite::memory:'))->exec($s);";
    assert!(!reaches(&file(true, "string $s", call), "f").0);
    assert!(reaches(&file(false, "string $s", call), "f").0);
}

/// The cost of curating `PDO::__construct`'s `$options` as `Autoload`: the array
/// can carry a statement class, which is looked up, so no shape rules it out.
#[test]
fn a_pdo_options_array_reaches_the_autoloader() {
    let call = "return new \\PDO('sqlite::memory:', null, null, [\\PDO::ATTR_ERRMODE => 2]);";
    assert!(reaches(&file(true, "", call), "f").0);
    assert!(reaches(&file(false, "", call), "f").0);
    let call = "return new \\PDO('sqlite::memory:');";
    assert!(!reaches(&file(true, "", call), "f").0);
    assert!(!reaches(&file(false, "", call), "f").0);
}

/// A call's value is read off the callee's declared return (issue #877, row 8.3):
/// `sprintf` declares a `string`, so the message is no object in either file. A
/// call whose declared return proves nothing keeps the gap in a coercive file and
/// leaves a strict one alone.
#[test]
fn a_sprintf_message_is_a_string_by_its_declared_return() {
    let call = "return new \\RuntimeException(sprintf('%s', $s));";
    assert_eq!(reaches(&file(false, "string $s", call), "f"), NEITHER);
    assert_eq!(reaches(&file(true, "string $s", call), "f"), NEITHER);
    for message in ["current($a)", "json_decode($s)"] {
        let call = format!("return new \\RuntimeException({message});");
        assert_eq!(reaches(&file(false, "string $s, array $a", &call), "f"), BOTH, "{message}");
    }
    // A literal, a concatenation and an interpolation are strings by their form.
    for message in ["'plain'", "'a' . $s", "\"a $s\""] {
        let call = format!("return new \\RuntimeException({message});");
        assert_eq!(reaches(&file(false, "string $s", &call), "f"), NEITHER, "{message}");
    }
}

#[test]
fn a_named_argument_list_reads_blind() {
    let call = "return new \\RuntimeException(message: $s);";
    assert_eq!(reaches(&file(false, "string $s", call), "f"), BOTH);
    assert_eq!(reaches(&file(true, "string $s", call), "f"), NEITHER);
}

#[test]
fn a_throwable_accessor_stays_exhaustive() {
    let src = "<?php\n\
        class Oops extends \\Exception {\n\
            public function read(): string { return $this->getMessage(); }\n\
        }\n\
        function g(\\Throwable $e): string { return $e->getMessage(); }\n";
    for symbol in ["Oops::read", "g"] {
        let s = summary(src, symbol);
        assert!(s.exhaustive && s.labels.is_empty(), "{symbol}: {s:?}");
    }
}

/// `PDOStatement::setFetchMode` and `PDO::setAttribute` are where a fetch's user
/// code is attributed (ADR-0099 §4.5, issue #870): the class `FETCH_CLASS` names,
/// the object `FETCH_INTO` writes through and the statement class
/// `ATTR_STATEMENT_CLASS` registers reach the engine at the registering call,
/// strict files included, while the fetches keep the rows they had. Neither
/// method has a throw row, so the throw lane stays a gap there: the effect lane
/// is the one these tests read.
mod registration {
    use super::summary;

    /// Whether `symbol` reaches user code through an operand in the effect lane.
    fn reaches(src: &str, symbol: &str) -> bool {
        summary(src, symbol).gaps.contains(&"user-code-reach")
    }

    /// A `PDOStatement` subclass whose `run` makes `statement`, and a `PDO`
    /// subclass whose `connect` makes `connection`: the exact receivers a
    /// project wrapper writes.
    fn wrapper(strict: bool, signature: &str, statement: &str, connection: &str) -> String {
        let declare = if strict { "declare(strict_types=1);\n" } else { "" };
        format!(
            "<?php\n{declare}\
             final class Row {{ public function __set(string $n, mixed $v): void {{ throw new \\LogicException('x'); }} }}\n\
             class St extends \\PDOStatement {{\n\
                 public function run({signature}): mixed {{ {statement} }}\n\
             }}\n\
             class Db extends \\PDO {{\n\
                 public function connect({signature}): mixed {{ {connection} }}\n\
             }}\n"
        )
    }

    #[test]
    fn a_registered_class_name_reaches_the_autoloader_in_both_modes() {
        let call = "return parent::setFetchMode(\\PDO::FETCH_CLASS, $c);";
        for strict in [false, true] {
            let src = wrapper(strict, "string $c", call, "");
            assert!(reaches(&src, "St::run"), "strict={strict}");
        }
        // A literal name is still looked up, so the body that registers it holds the gap.
        let call = "return parent::setFetchMode(\\PDO::FETCH_CLASS, 'Row', []);";
        assert!(reaches(&wrapper(true, "", call, ""), "St::run"));
        // A bound receiver (`$this`) is a gap already: a subclass may override the
        // method, and only a final one answers there.
        let call = "return $this->setFetchMode(\\PDO::FETCH_CLASS, $c);";
        let s = summary(&wrapper(false, "string $c", call, ""), "St::run");
        assert!(!s.exhaustive && s.labels.is_empty(), "{s:?}");
    }

    #[test]
    fn a_registered_object_reaches_through_its_set() {
        let call = "return parent::setFetchMode(\\PDO::FETCH_INTO, $o);";
        assert!(reaches(&wrapper(true, "Row $o", call, ""), "St::run"));
        let call = "return parent::setFetchMode(\\PDO::FETCH_INTO, new Row());";
        assert!(reaches(&wrapper(false, "", call, ""), "St::run"));
    }

    /// `setFetchMode(FETCH_CLASS | FETCH_CLASSTYPE)` names no class: a column names
    /// it at every later fetch. The rule reads no constants, so the mode is
    /// `Autoload` too and no `setFetchMode` call is complete, whatever mode it names.
    #[test]
    fn every_fetch_mode_registration_is_a_gap() {
        for mode in ["\\PDO::FETCH_CLASS | \\PDO::FETCH_CLASSTYPE", "\\PDO::FETCH_ASSOC", "$m"] {
            let call = format!("return parent::setFetchMode({mode});");
            for strict in [false, true] {
                let src = wrapper(strict, "int $m", &call, "");
                assert!(reaches(&src, "St::run"), "{mode}, strict={strict}");
                let s = summary(&src, "St::run");
                assert!(!s.exhaustive, "{mode}, strict={strict}: {s:?}");
                assert_eq!(s.labels, ["mutate"], "the row is kept: {s:?}");
            }
        }
    }

    /// A fetch with no registration constructs nothing, so it stays as it was:
    /// no operand, no reach.
    #[test]
    fn a_fetch_with_no_prior_registration_reaches_nothing() {
        let call = "return parent::fetch();";
        let src = wrapper(false, "", call, "");
        assert!(!reaches(&src, "St::run"));
        assert_eq!(summary(&src, "St::run").labels, ["io.db"]);
    }

    #[test]
    fn a_statement_class_attribute_reaches_the_autoloader() {
        let call = "return parent::setAttribute(\\PDO::ATTR_STATEMENT_CLASS, [$c]);";
        for strict in [false, true] {
            let src = wrapper(strict, "string $c", "", call);
            assert!(reaches(&src, "Db::connect"), "strict={strict}");
            let s = summary(&src, "Db::connect");
            assert_eq!(s.labels, ["io.db"], "the row is kept: {s:?}");
        }
    }

    /// `ATTR_ERRMODE` is no class, but the value is an unproven `mixed` and the
    /// rule does not read constants: any value reaches.
    #[test]
    fn an_attribute_value_is_never_ruled_out() {
        let call = "return parent::setAttribute(\\PDO::ATTR_ERRMODE, \\PDO::ERRMODE_EXCEPTION);";
        assert!(reaches(&wrapper(true, "", "", call), "Db::connect"));
    }
}
