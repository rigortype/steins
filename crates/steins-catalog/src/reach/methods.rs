//! The **user code an engine method or constructor can reach through its
//! arguments** (issue #858, ADR-0099 §4.2): [`method_arg_reach`], the
//! `Class::method`-keyed twin of [`arg_reach`](crate::arg_reach).
//!
//! `new \RuntimeException($o)`, `$pdo->query($o)` and `new DateTime($o)`
//! convert an object argument through `__toString`, and `new PDO(…, $options)`
//! can name a class the autoloader then loads. The method rows of the effect
//! and throw axes answer without looking at the arguments, so the call-site
//! rule of ADR-0021's second 2026-10-01 amendment needs a row here for each
//! of them.
//!
//! # Which methods have a row
//!
//! Exactly the methods that already have a row on another axis, and no
//! others: [`method_effect_labels`](crate::method_effect_labels) (`PDO`,
//! `PDOStatement`, `DateTime` and `DateTimeImmutable`, the catalogued
//! containers' constructors, every engine `Throwable`'s constructor and
//! accessors), [`final_method_effect_labels`](crate::final_method_effect_labels)
//! (a subset of the same) and [`method_throws`](crate::method_throws) (a
//! subset again). A test holds the three in lockstep with this table.
//!
//! # How a row is keyed
//!
//! The way those tables are: by the **global engine class the call resolves
//! to**, case-insensitively (a leading `\` is ignored), never by walking up
//! the hierarchy here. A caller that sees `new MyException($o)` with a
//! project class resolves it to its nearest engine ancestor first, as it does
//! for the effect and throw rows, and asks for that class. The one family
//! answered by ancestry is the engine `Throwable`s: all 60 of them (PHP
//! 8.5.11) are rowed by [`is_builtin_throwable`], and 56 share
//! `Exception::__construct`'s signature (`Error::__construct` has the same
//! parameters), so `RuntimeException`, `TypeError` and `JsonException` all
//! take the default row. The four with their own signature are keyed by name:
//! `ErrorException`, `FiberError`, `SoapFault` and
//! `Uri\WhatWg\InvalidUrlException`.
//!
//! # A rowed method with no row here reaches blind
//!
//! `None` means **the catalog states nothing about this method's arguments**,
//! and a caller must read it as "may reach anything": a call that keeps the
//! method's other rows and is a coverage gap. It is never a promise of
//! inertness. Only `Some` row, whose `Inert` positions are curated, rules an
//! argument out. A method with no row on any axis is `None` for the plainer
//! reason that the catalog has never heard of it.
//!
//! # Derivation and overrides
//!
//! The declared parameter types are written out by hand, from
//! `ReflectionMethod` on PHP 8.5.11 with the extension set the function table
//! was mined over (no method parameter facts are mined, unlike
//! [`param_facts`](crate::param_facts)), and give each position the same
//! reach as [`arg_reach`](crate::arg_reach)'s derivation. A short override
//! list then corrects it where php-src does less or more than the type says.
//! Every override and every `Coerced` claim was witnessed on 8.5.11 in both
//! calling modes with an object carrying `__toString`, `__get`, `__set`,
//! `__clone`, `__debugInfo`, `__serialize`, `Countable`, `ArrayAccess`,
//! `IteratorAggregate` and `JsonSerializable`:
//!
//! * `Coerced` `string` parameters run `__toString` under coercive typing
//!   and are a `TypeError` under `strict_types=1`. Witnessed for `$message`
//!   of every irregular constructor, `$filename`, `PDO::__construct`'s
//!   `$dsn`, `$username` and `$password`, `PDO::query`, `exec` and `prepare`,
//!   the `DateTime` and `DateTimeImmutable` constructors and
//!   `createFromFormat`, and `DateInterval::__construct`.
//! * An **internal-class-typed** parameter (`?Throwable $previous`,
//!   `?DateTimeZone $timezone`, `DateTimeImmutable`, `DateTime` and
//!   `DateTimeInterface` objects) is `Inert`, although the derivation says
//!   `Object`. A user subclass of an internal class cannot be a lazy object
//!   (`newLazyGhost` refuses it), and passing one with every magic method
//!   overridden, `format`, `getTimestamp` and `getName` included, ran
//!   nothing in either mode. `Throwable` is implementable only through
//!   `Exception` and `Error`, so it is such a class too.
//! * `ArrayObject` and `ArrayIterator`'s `$array` is `Inert`: an object that
//!   overrides `getIterator`, `offsetGet`, `count`, `__get` and `__toString`,
//!   a lazy plain object, an `ArrayObject` wrapping a trap object and an
//!   array of objects all ran nothing at construction (the `Nested`
//!   derivation of `object|array`). `ArrayObject`'s `$iteratorClass` is a
//!   class name that is looked up (`Autoload`): a missing name autoloads in
//!   both modes, and an object there is converted first.
//! * `PDO::__construct`'s and `PDO::prepare`'s `$options` can carry
//!   `PDO::ATTR_STATEMENT_CLASS => ['Name']`, which looks `Name` up
//!   (`Autoload`) and, for `prepare`, runs a user statement class's
//!   constructor. A literal array of strings is the same shape as one that
//!   holds no such key, so the position is `Autoload` and never ruled out.
//!   `PDO::query`'s `mixed ...$fetchModeArgs` names a row class for
//!   `FETCH_CLASS` (autoloaded, both modes) or an object for `FETCH_INTO`
//!   whose `__set` the later `fetch` runs: `Autoload`. `FETCH_FUNC` is
//!   refused there.
//! * `PDOStatement::fetchAll`'s `mixed ...$args` (positions 1 and up) is the
//!   callable of `FETCH_FUNC` (witnessed to run, both modes) or the class of
//!   `FETCH_CLASS` (autoloaded): `Callback`. The mode is a runtime integer,
//!   so `fetchAll(PDO::FETCH_COLUMN, 0)` is not told apart from either.
//!   `PDOStatement::execute`'s `$params` converts an object value through
//!   `__toString` in both modes, as the `Nested` derivation says.
//! * `SoapFault`'s `$details` and `$headerFault` (`mixed`) are stored:
//!   `Inert`, with an object, a lazy object, a closure and an array of
//!   objects. A `$code` array's elements are checked, not converted
//!   (`ValueError`), and stays at the derived `Nested`.
//! * The `Throwable` accessors and the constructors of classes that declare
//!   none (`stdClass`, the SPL lists and heaps, `SplObjectStorage`,
//!   `WeakMap`) have no parameters. `new SplStack($o)` evaluates its
//!   arguments and discards them, an accessor given one raises an
//!   `ArgumentCountError` before running, and a position past the list is
//!   `Inert`.
//!
//! The row answers what the **arguments** can reach and nothing else, as
//! [`arg_reach`](crate::arg_reach) does. User code that a method's own design
//! runs, whatever it was given, is not described by it: a `PDOStatement`
//! fetched in `FETCH_CLASS` mode constructs the class `setFetchMode` named,
//! and a statement class registered earlier runs at `prepare`.

use super::{ArgReach, declared_reach, reaches_past_strictness};
use crate::effects::{THROWABLE_ACCESSORS, is_builtin_throwable};
use ArgReach::{Autoload, Callback, Inert};

/// One engine method's [`ArgReach`] row ([`method_arg_reach`]).
#[derive(Debug, Clone, Copy)]
pub struct MethodReachRow {
    /// The declared parameter types, as `ReflectionMethod` prints them.
    params: &'static [&'static str],
    /// Whether the last parameter is variadic.
    variadic: bool,
    /// Where the derivation from `params` is corrected; positions are 0-based.
    overrides: &'static [(usize, ArgReach)],
}

impl MethodReachRow {
    /// The reach of the argument at `position`.
    #[must_use]
    pub fn at(&self, position: usize) -> ArgReach {
        let len = self.params.len();
        let index = match len.checked_sub(1) {
            Some(last) if position > last && self.variadic => last,
            _ if position < len => position,
            _ => return ArgReach::Inert,
        };
        match self.overrides.iter().find(|(p, _)| *p == index) {
            Some(&(_, reach)) => reach,
            None => declared_reach(self.params[index]),
        }
    }

    /// Whether some position reaches user code that the calling file's
    /// strictness alone does not rule out: the question for a call whose
    /// positions cannot be read, a named or spread argument list.
    #[must_use]
    pub fn reaches_blind(&self, strict: bool) -> bool {
        (0..self.params.len()).any(|p| reaches_past_strictness(self.at(p), strict))
    }
}

/// The reach row of the method `method` on the engine class `class` (both
/// case-insensitive, `class` global and without a leading `\`), or `None`.
///
/// **`None` is "blind", not "inert".** A method the effect and throw tables
/// row has a row here; a caller that gets `None` for one of them must treat
/// its arguments as able to reach any user code, and keep whatever the other
/// axes say. The module documentation says which methods are rowed, how a
/// class is keyed and what each override was witnessed against.
#[must_use]
pub fn method_arg_reach(class: &str, method: &str) -> Option<MethodReachRow> {
    let class = class.trim_start_matches('\\').to_ascii_lowercase();
    let method = method.to_ascii_lowercase();
    match (class.as_str(), method.as_str()) {
        ("pdo", "__construct") => {
            row(&["string", "?string", "?string", "?array"], false, &[(3, Autoload)])
        }
        ("pdo", "query") => row(&["string", "?int", "mixed"], true, &[(2, Autoload)]),
        ("pdo", "exec") => row(&["string"], false, &[]),
        ("pdo", "prepare") => row(&["string", "array"], false, &[(1, Autoload)]),
        ("pdostatement", "execute") => row(&["?array"], false, &[]),
        ("pdostatement", "fetch") => row(&["int", "int", "int"], false, &[]),
        ("pdostatement", "fetchall") => row(&["int", "mixed"], true, &[(1, Callback)]),
        ("datetime" | "datetimeimmutable", "__construct") => {
            row(&["string", "?DateTimeZone"], false, &[(1, Inert)])
        }
        ("datetime" | "datetimeimmutable", "createfromformat") => {
            row(&["string", "string", "?DateTimeZone"], false, &[(2, Inert)])
        }
        ("datetime", "createfromimmutable") => row(&["DateTimeImmutable"], false, &[(0, Inert)]),
        ("datetimeimmutable", "createfrommutable") => row(&["DateTime"], false, &[(0, Inert)]),
        ("datetime" | "datetimeimmutable", "createfrominterface") => {
            row(&["DateTimeInterface"], false, &[(0, Inert)])
        }
        ("dateinterval", "__construct") => row(&["string"], false, &[]),
        ("arrayobject", "__construct") => {
            row(&["object|array", "int", "string"], false, &[(0, Inert), (2, Autoload)])
        }
        ("arrayiterator", "__construct") => row(&["object|array", "int"], false, &[(0, Inert)]),
        ("splfixedarray", "__construct") => row(&["int"], false, &[]),
        (
            "stdclass" | "spldoublylinkedlist" | "splstack" | "splqueue" | "splobjectstorage"
            | "splpriorityqueue" | "splminheap" | "splmaxheap" | "weakmap",
            "__construct",
        ) => row(&[], false, &[]),
        (_, "__construct") if is_builtin_throwable(&class) => throwable_constructor(&class),
        (_, m) if THROWABLE_ACCESSORS.contains(&m) && is_builtin_throwable(&class) => {
            row(&[], false, &[])
        }
        _ => None,
    }
}

/// The constructor of the engine `Throwable` `class`: the four irregular
/// signatures by name, `Exception::__construct`'s for the rest.
fn throwable_constructor(class: &str) -> Option<MethodReachRow> {
    match class {
        // `__construct(string $message, int $code, int $severity, ?string
        // $filename, ?int $line, ?Throwable $previous)`.
        "errorexception" => row(
            &["string", "int", "int", "?string", "?int", "?Throwable"],
            false,
            &[(5, Inert)],
        ),
        // Takes no parameters and refuses to be constructed (`Error`, whatever
        // it is given).
        "fibererror" => row(&[], false, &[]),
        "uri\\whatwg\\invalidurlexception" => {
            row(&["string", "array", "int", "?Throwable"], false, &[(3, Inert)])
        }
        "soapfault" => row(
            &["array|string|null", "string", "?string", "mixed", "?string", "mixed", "string"],
            false,
            &[(3, Inert), (5, Inert)],
        ),
        _ => row(&["string", "int", "?Throwable"], false, &[(2, Inert)]),
    }
}

const fn row(
    params: &'static [&'static str],
    variadic: bool,
    overrides: &'static [(usize, ArgReach)],
) -> Option<MethodReachRow> {
    Some(MethodReachRow { params, variadic, overrides })
}

#[cfg(test)]
mod tests {
    use super::method_arg_reach;
    use crate::effects::is_builtin_throwable;
    use crate::{
        ArgReach, builtin_class_supers, final_method_effect_labels, method_effect_labels,
        method_throws,
    };

    fn at(class: &str, method: &str, position: usize) -> ArgReach {
        method_arg_reach(class, method)
            .unwrap_or_else(|| panic!("{class}::{method} has no row"))
            .at(position)
    }

    #[test]
    fn the_throwable_constructors_coerce_only_the_message() {
        for class in ["Exception", "RuntimeException", "Error", "TypeError", "JsonException"] {
            assert_eq!(at(class, "__construct", 0), ArgReach::Coerced, "{class}");
            assert_eq!(at(class, "__construct", 1), ArgReach::Inert, "{class}: a TypeError");
            assert_eq!(at(class, "__construct", 2), ArgReach::Inert, "{class}: a type check");
            assert_eq!(at(class, "__construct", 3), ArgReach::Inert, "{class}: past the list");
        }
        assert_eq!(at("DOMException","__construct", 0), ArgReach::Coerced);
        assert_eq!(at("\\runtimeexception", "__CONSTRUCT", 0), ArgReach::Coerced);
    }

    #[test]
    fn a_throwable_with_its_own_signature_has_its_own_row() {
        assert_eq!(at("ErrorException", "__construct", 3), ArgReach::Coerced, "$filename");
        assert_eq!(at("ErrorException", "__construct", 2), ArgReach::Inert, "$severity");
        assert_eq!(at("ErrorException", "__construct", 5), ArgReach::Inert, "$previous");
        assert_eq!(at("FiberError", "__construct", 0), ArgReach::Inert);
        assert_eq!(at("SoapFault", "__construct", 0), ArgReach::Nested, "array|string|null");
        assert_eq!(at("SoapFault", "__construct", 1), ArgReach::Coerced);
        assert_eq!(at("SoapFault", "__construct", 3), ArgReach::Inert, "$details is stored");
        assert_eq!(at("SoapFault", "__construct", 6), ArgReach::Coerced, "$lang");
        let url = "Uri\\WhatWg\\InvalidUrlException";
        assert_eq!(at(url, "__construct", 0), ArgReach::Coerced);
        assert_eq!(at(url, "__construct", 1), ArgReach::Nested, "$errors");
        assert_eq!(at(url, "__construct", 3), ArgReach::Inert);
    }

    #[test]
    fn the_throwable_accessors_take_no_arguments() {
        for method in ["getMessage", "getCode", "getFile", "getLine", "getPrevious", "getTrace"] {
            let row = method_arg_reach("RuntimeException", method).expect(method);
            assert_eq!(row.at(0), ArgReach::Inert, "{method}");
            assert!(!row.reaches_blind(false), "{method}");
        }
        assert!(method_arg_reach("Error", "getTraceAsString").is_some());
        assert!(method_arg_reach("RuntimeException", "__toString").is_none(), "not final");
    }

    #[test]
    fn the_date_constructors_and_factories_coerce_their_strings() {
        for class in ["DateTime", "DateTimeImmutable"] {
            assert_eq!(at(class, "__construct", 0), ArgReach::Coerced, "{class}");
            assert_eq!(at(class, "__construct", 1), ArgReach::Inert, "{class}: ?DateTimeZone");
            assert_eq!(at(class, "createFromFormat", 0), ArgReach::Coerced, "{class}");
            assert_eq!(at(class, "createFromFormat", 1), ArgReach::Coerced, "{class}");
            assert_eq!(at(class, "createFromFormat", 2), ArgReach::Inert, "{class}");
            assert_eq!(at(class, "createFromInterface", 0), ArgReach::Inert, "{class}");
        }
        assert_eq!(at("DateTime", "createFromImmutable", 0), ArgReach::Inert);
        assert_eq!(at("DateTimeImmutable", "createFromMutable", 0), ArgReach::Inert);
        assert!(method_arg_reach("DateTime", "createFromMutable").is_none(), "wrong class");
        assert_eq!(at("DateInterval", "__construct", 0), ArgReach::Coerced);
    }

    #[test]
    fn pdo_coerces_its_sql_and_autoloads_what_its_options_name() {
        assert_eq!(at("PDO", "query", 0), ArgReach::Coerced);
        assert_eq!(at("PDO", "query", 1), ArgReach::Inert, "?int $fetchMode");
        assert_eq!(at("PDO", "query", 2), ArgReach::Autoload, "FETCH_CLASS names a class");
        assert_eq!(at("PDO", "query", 5), ArgReach::Autoload, "the variadic tail repeats");
        assert_eq!(at("PDO", "exec", 0), ArgReach::Coerced);
        assert_eq!(at("PDO", "prepare", 0), ArgReach::Coerced);
        assert_eq!(at("PDO", "prepare", 1), ArgReach::Autoload, "ATTR_STATEMENT_CLASS");
        assert_eq!(at("PDO", "__construct", 0), ArgReach::Coerced);
        assert_eq!(at("PDO", "__construct", 2), ArgReach::Coerced);
        assert_eq!(at("PDO", "__construct", 3), ArgReach::Autoload);
        assert_eq!(at("PDOStatement", "execute", 0), ArgReach::Nested);
        assert_eq!(at("PDOStatement", "fetch", 0), ArgReach::Inert);
        assert_eq!(at("PDOStatement", "fetchAll", 0), ArgReach::Inert);
        assert_eq!(at("PDOStatement", "fetchAll", 1), ArgReach::Callback, "FETCH_FUNC");
        assert_eq!(at("PDOStatement", "fetchAll", 4), ArgReach::Callback);
    }

    #[test]
    fn the_containers_reach_only_through_a_class_name() {
        assert_eq!(at("ArrayObject", "__construct", 0), ArgReach::Inert);
        assert_eq!(at("ArrayObject", "__construct", 1), ArgReach::Inert);
        assert_eq!(at("ArrayObject", "__construct", 2), ArgReach::Autoload);
        assert_eq!(at("ArrayIterator", "__construct", 0), ArgReach::Inert);
        assert_eq!(at("SplFixedArray", "__construct", 0), ArgReach::Inert);
        for class in ["stdClass", "SplStack", "SplObjectStorage", "WeakMap", "SplMinHeap"] {
            let row = method_arg_reach(class, "__construct").expect(class);
            assert_eq!(row.at(0), ArgReach::Inert, "{class}: no constructor to see it");
            assert!(!row.reaches_blind(false), "{class}");
        }
    }

    #[test]
    fn a_method_without_a_row_is_blind_not_inert() {
        assert!(method_arg_reach("PDO", "setAttribute").is_none());
        assert!(method_arg_reach("PDOStatement", "bindValue").is_none());
        assert!(method_arg_reach("Foo", "__construct").is_none(), "not an engine class");
        assert!(method_arg_reach("DateTime", "modify").is_none());
    }

    #[test]
    fn an_unreadable_argument_list_reaches_what_strictness_leaves() {
        let message = method_arg_reach("RuntimeException", "__construct").expect("row");
        assert!(message.reaches_blind(false));
        assert!(!message.reaches_blind(true), "a coerced string is a TypeError there");
        assert!(method_arg_reach("PDO", "prepare").expect("row").reaches_blind(true));
        assert!(!method_arg_reach("PDO", "exec").expect("row").reaches_blind(true));
    }

    /// The reach table and the effect and throw tables rows the same methods:
    /// a method added to one without the other would be blind, or have a row
    /// for a method nothing else knows.
    #[test]
    fn the_reach_rows_are_the_effect_and_throw_rows() {
        const METHODS: &[&str] = &[
            "__construct", "__toString", "query", "exec", "prepare", "execute", "fetch",
            "fetchAll", "setAttribute", "createFromFormat", "createFromImmutable",
            "createFromMutable", "createFromInterface", "modify", "format", "getMessage",
            "getCode", "getFile", "getLine", "getPrevious", "getTrace", "getTraceAsString",
        ];
        let mut classes: Vec<&str> =
            crate::hierarchy_generated::HIERARCHY.iter().map(|r| r.0).collect();
        classes.extend(["stdClass", "WeakMap", "DateInterval", "PDO", "PDOStatement"]);
        for class in classes {
            for method in METHODS {
                let rowed = method_effect_labels(class, method).is_some()
                    || final_method_effect_labels(class, method).is_some()
                    || method_throws(class, method).is_some();
                assert_eq!(
                    method_arg_reach(class, method).is_some(),
                    rowed,
                    "{class}::{method}: reach and effect/throw rows disagree",
                );
            }
        }
    }

    /// Every engine `Throwable` has a constructor row, and only the irregular
    /// four differ from the default one.
    #[test]
    fn every_engine_throwable_has_a_constructor_row() {
        let mut count = 0;
        for (class, _) in crate::hierarchy_generated::HIERARCHY {
            if !is_builtin_throwable(class) || class.eq_ignore_ascii_case("throwable") {
                continue;
            }
            count += 1;
            let row = method_arg_reach(class, "__construct").unwrap_or_else(|| panic!("{class}"));
            let irregular =
                ["errorexception", "fibererror", "soapfault", "uri\\whatwg\\invalidurlexception"];
            if irregular.contains(class) {
                continue;
            }
            assert_eq!(row.at(0), ArgReach::Coerced, "{class}");
            assert_eq!(row.at(1), ArgReach::Inert, "{class}");
            assert_eq!(row.at(2), ArgReach::Inert, "{class}");
            assert_eq!(row.at(3), ArgReach::Inert, "{class}");
        }
        assert!(count >= 60, "{count} engine Throwables in the hierarchy");
        assert!(builtin_class_supers("RuntimeException").is_some());
    }
}
