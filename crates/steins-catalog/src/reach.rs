//! The **user code a builtin can reach through its arguments** (issue #856,
//! ADR-0021's second 2026-10-01 amendment): [`arg_reach`] and its per-position
//! answer, [`ArgReach`].
//!
//! A builtin whose own effects are certified can still hand control to
//! userland through what it was given. A coercive `string` parameter converts
//! an object through `__toString`; `count()` calls `Countable::count`;
//! `in_array()` compares loosely and `implode()` renders each piece; a
//! callable runs; `is_callable('Foo::bar')` autoloads. Each of those is the
//! callee running code the effects pass never saw, so a call is pure only
//! where the call site rules every reaching argument out.
//!
//! The row is **derived from the mined arginfo** ([`param_facts`]), and a short
//! curated list overrides the derivation where php-src shows a builtin does
//! less with a parameter than its type admits. The derivation, per position:
//!
//! | Declared member | Reach | Why |
//! |---|---|---|
//! | `int`, `float`, `bool`, `false`, `null`, … | [`ArgReach::Inert`] | a `TypeError` |
//! | `string` | [`ArgReach::Coerced`] | `__toString`, under coercive typing |
//! | a class, an interface, `object`, `iterable` | [`ArgReach::Object`] | used as itself |
//! | `array`, `mixed` | [`ArgReach::Nested`] | the builtin's own business |
//! | a declared `callable` position | [`ArgReach::Callback`] | it runs |
//!
//! A union takes its strongest member's reach, a variadic tail repeats its
//! last position, and a position past a non-variadic list is an
//! `ArgumentCountError` raised before anything runs. A position the
//! resource table says demands a resource is inert: an object there is a
//! `TypeError`. The certified pure families answer inert everywhere, which is
//! what certified means.
//!
//! Witnessed on PHP 8.5.11 for every override and every reach kind, in both
//! calling modes: `strlen($o)` runs `__toString` and is a `TypeError` under
//! `strict_types=1`; `strval($o)`, `sprintf('%s', $o)`, `implode(',', [$o])`,
//! `in_array('x', [$o])` and `json_encode([$j])` run user code in both modes;
//! `intval($o)`, `boolval($o)`, `gettype($o)`, `array_fill(0, 1, $o)`,
//! `array_merge([$o])` and `count([$c], COUNT_RECURSIVE)` run none.
//!
//! [`param_facts`]: crate::param_facts

use crate::builtins::{ParamFacts, resource_param};
use crate::effects::certified_pure;

/// What user code a builtin can reach through one argument position, from
/// none to the most, and what rules it out at a call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArgReach {
    /// None, for every value the parameter admits.
    Inert,
    /// A coercive `string` parameter: an object argument is converted through
    /// its `__toString`. Ruled out by an argument shown not to be an object,
    /// or by `declare(strict_types=1)` in the calling file, where the object
    /// is a `TypeError` instead.
    Coerced,
    /// The builtin converts, counts or reads an object argument itself, in
    /// either calling mode (`strval`'s `__toString`, `count`'s
    /// `Countable::count`, a lazy object's initializer). Ruled out by an
    /// argument shown not to be an object.
    Object,
    /// The builtin also converts or compares what an array argument holds
    /// (`implode`'s pieces, `in_array`'s haystack, `json_encode`'s values).
    /// Ruled out only by an argument shown to hold no object at any depth.
    Nested,
    /// A callable: the call runs it. Never ruled out at a call site the
    /// effects pass sees as a plain call; a callback it can resolve takes
    /// another road.
    Callback,
    /// A class or method named in a string is looked up, which runs the
    /// autoloader. Never ruled out.
    Autoload,
}

/// One builtin's [`ArgReach`] row ([`arg_reach`]).
#[derive(Debug, Clone, Copy)]
pub struct ReachRow {
    facts: &'static ParamFacts,
    name: &'static str,
    certified: bool,
}

impl ReachRow {
    /// The reach of the argument at `position`.
    #[must_use]
    pub fn at(&self, position: usize) -> ArgReach {
        if self.certified {
            return ArgReach::Inert;
        }
        let last = self.facts.params.len().checked_sub(1);
        let index = match last {
            Some(last) if position > last && self.facts.variadic.contains(&last) => last,
            _ if position < self.facts.params.len() => position,
            _ => return ArgReach::Inert,
        };
        if let Some(&(_, reach)) = overrides(self.name).iter().find(|(p, _)| *p == index) {
            return reach;
        }
        if self.facts.callable.contains(&index) {
            return ArgReach::Callback;
        }
        if resource_param(self.name, index).is_some() {
            return ArgReach::Inert;
        }
        declared_reach(self.facts.params[index])
    }

    /// Whether some position reaches user code that the calling file's
    /// strictness alone does not rule out: the question for a call whose
    /// positions cannot be read, a named or spread argument list or a
    /// builtin handed over as a callback.
    #[must_use]
    pub fn reaches_blind(&self, strict: bool) -> bool {
        (0..self.facts.params.len()).any(|p| match self.at(p) {
            ArgReach::Inert => false,
            ArgReach::Coerced => !strict,
            _ => true,
        })
    }
}

/// The reach row of the builtin `name` (case-insensitive), or `None` when the
/// mining build had no such function.
///
/// The row answers what the **arguments** can reach and nothing else: a
/// builtin that runs user code by its own design (`unserialize`'s magic
/// methods, `spl_autoload_call`) is not described by it. The effects pass asks
/// it only of a name whose own effects the catalog already states.
#[must_use]
pub fn arg_reach(name: &str) -> Option<ReachRow> {
    let key = name.trim_start_matches('\\').to_ascii_lowercase();
    let rows = crate::param_facts_generated::PARAM_FACTS;
    let i = rows.binary_search_by(|(n, _)| (*n).cmp(key.as_str())).ok()?;
    let (name, facts) = &rows[i];
    Some(ReachRow { facts, name, certified: certified_pure(name) })
}

/// The reach a declared type admits ([`ArgReach`]'s derivation table).
fn declared_reach(ty: &str) -> ArgReach {
    ty.split('|')
        .map(|member| member.trim().trim_start_matches('?'))
        .map(|member| match member.to_ascii_lowercase().as_str() {
            "int" | "float" | "bool" | "true" | "false" | "null" => ArgReach::Inert,
            "string" => ArgReach::Coerced,
            "array" | "mixed" => ArgReach::Nested,
            _ => ArgReach::Object,
        })
        .max()
        .unwrap_or(ArgReach::Nested)
}

/// The curated rows of `name`.
fn overrides(name: &str) -> &'static [(usize, ArgReach)] {
    OVERRIDES.iter().find(|(n, _)| *n == name).map_or(&[], |(_, row)| row)
}

use ArgReach::{Autoload, Callback, Inert, Object};

/// Where php-src does less with a parameter than its declared type admits,
/// or where the derivation cannot see what it does. Each row is witnessed in
/// both calling modes on PHP 8.5.11; positions are 0-based.
const OVERRIDES: &[(&str, &[(usize, ArgReach)])] = &[
    // A `mixed` value read only for its type tag, or converted by a handler
    // that has no userland hook: a userland object converts to `int` or
    // `float` with a warning and to `bool` as `true`, without `__toString`.
    ("boolval", &[(0, Inert)]),
    ("doubleval", &[(0, Inert)]),
    ("floatval", &[(0, Inert)]),
    ("gettype", &[(0, Inert)]),
    ("intval", &[(0, Inert)]),
    // A value stored or copied, never converted or compared.
    ("array_fill", &[(2, Inert)]),
    ("array_filter", &[(0, Inert)]),
    ("array_merge", &[(0, Inert)]),
    ("array_pop", &[(0, Inert)]),
    ("array_push", &[(0, Inert), (1, Inert)]),
    ("array_shift", &[(0, Inert)]),
    ("shuffle", &[(0, Inert)]),
    ("array_unshift", &[(0, Inert), (1, Inert)]),
    // `array_splice` converts a non-array replacement to an array. Kept at
    // `Object`, the reading that rules out least, although no user code was
    // seen to run there (a lazy ghost stays uninitialized).
    ("array_splice", &[(0, Inert), (3, Object)]),
    // `count` calls `Countable::count` on an object and never looks inside an
    // array, `COUNT_RECURSIVE` included.
    ("count", &[(0, Object)]),
    ("sizeof", &[(0, Object)]),
    // A rendered value: an object runs `__toString`, an array renders as
    // `Array` without its elements being read.
    ("printf", &[(1, Object)]),
    ("sprintf", &[(1, Object)]),
    ("strval", &[(0, Object)]),
    ("settype", &[(0, Object)]),
    // An internal enum, which userland cannot extend.
    ("round", &[(2, Inert)]),
    // The key sorts compare keys, which are never objects; the comparator
    // sorts hand the values to the callback and compare nothing themselves.
    ("krsort", &[(0, Inert)]),
    ("ksort", &[(0, Inert)]),
    ("uasort", &[(0, Inert)]),
    ("uksort", &[(0, Inert)]),
    ("usort", &[(0, Inert)]),
    // The internal pointer moves read an object's property table, which runs
    // a lazy object's initializer; an array is not read.
    ("end", &[(0, Object)]),
    ("next", &[(0, Object)]),
    ("prev", &[(0, Object)]),
    ("reset", &[(0, Object)]),
    // `array_walk` hands values to its callback; an object's property table
    // is read, as above. Its extra argument goes to the callback untouched.
    ("array_walk", &[(0, Object), (2, Inert)]),
    ("array_walk_recursive", &[(0, Object), (2, Inert)]),
    // The invokers hand values to their callback without converting them.
    ("array_all", &[(0, Inert)]),
    ("array_any", &[(0, Inert)]),
    ("array_find", &[(0, Inert)]),
    ("array_find_key", &[(0, Inert)]),
    ("array_map", &[(1, Inert), (2, Inert)]),
    ("array_reduce", &[(0, Inert), (2, Inert)]),
    ("call_user_func", &[(1, Inert)]),
    ("call_user_func_array", &[(1, Inert)]),
    ("iterator_apply", &[(2, Inert)]),
    ("register_shutdown_function", &[(1, Inert)]),
    // The values of `preg_replace_callback_array`'s map are callables.
    ("preg_replace_callback_array", &[(0, Callback)]),
    // `is_callable` resolves a class named in a string or array.
    ("is_callable", &[(0, Autoload), (2, Inert)]),
    // By-reference out-parameters whose incoming value is discarded unread.
    ("preg_match", &[(2, Inert)]),
    ("preg_match_all", &[(2, Inert)]),
    ("preg_replace", &[(4, Inert)]),
    ("preg_replace_callback", &[(4, Inert)]),
    ("similar_text", &[(2, Inert)]),
    ("str_ireplace", &[(3, Inert)]),
    ("str_replace", &[(3, Inert)]),
];

#[cfg(test)]
mod tests {
    use super::{ArgReach, OVERRIDES, arg_reach};
    use crate::{certified_at_call_site, effect_labels, foldable, param_facts};

    fn at(name: &str, position: usize) -> ArgReach {
        arg_reach(name).unwrap_or_else(|| panic!("{name} has no row")).at(position)
    }

    #[test]
    fn the_derivation_reads_the_declared_type() {
        assert_eq!(at("strlen", 0), ArgReach::Coerced);
        assert_eq!(at("range", 0), ArgReach::Coerced, "string|int|float coerces an object");
        assert_eq!(at("intdiv", 0), ArgReach::Inert);
        assert_eq!(at("in_array", 0), ArgReach::Nested);
        assert_eq!(at("in_array", 1), ArgReach::Nested);
        assert_eq!(at("in_array", 2), ArgReach::Inert);
        assert_eq!(at("implode", 0), ArgReach::Nested, "array|string takes the stronger member");
        assert_eq!(at("json_encode", 0), ArgReach::Nested);
        assert_eq!(at("array_filter", 1), ArgReach::Callback);
        assert_eq!(at("iterator_apply", 0), ArgReach::Object, "a Traversable runs its methods");
        assert_eq!(at("fwrite", 0), ArgReach::Inert, "a resource position");
    }

    #[test]
    fn the_overrides_hold_where_php_src_does_less() {
        assert_eq!(at("count", 0), ArgReach::Object, "a Countable, never an array's elements");
        assert_eq!(at("gettype", 0), ArgReach::Inert);
        assert_eq!(at("strval", 0), ArgReach::Object);
        assert_eq!(at("is_callable", 0), ArgReach::Autoload);
        assert_eq!(at("preg_match", 2), ArgReach::Inert, "an out-parameter is written, not read");
        assert_eq!(at("usort", 0), ArgReach::Inert, "the comparator sees the values");
        assert_eq!(at("ksort", 0), ArgReach::Inert);
        assert_eq!(at("shuffle", 0), ArgReach::Inert, "a permutation compares nothing");
        assert_eq!(at("sort", 0), ArgReach::Nested);
    }

    /// Every curated row names a mined function and positions it has.
    #[test]
    fn every_override_names_a_mined_position() {
        for (name, row) in OVERRIDES {
            let facts = param_facts(name).unwrap_or_else(|| panic!("{name} was not mined"));
            for (position, _) in *row {
                assert!(*position < facts.params.len(), "{name} has no position {position}");
            }
        }
    }

    #[test]
    fn a_variadic_tail_repeats_and_a_position_past_the_list_is_inert() {
        assert_eq!(at("sprintf", 5), ArgReach::Object);
        assert_eq!(at("array_merge", 7), ArgReach::Inert);
        assert_eq!(at("strlen", 3), ArgReach::Inert, "an ArgumentCountError runs nothing");
    }

    #[test]
    fn a_certified_name_reaches_nothing() {
        for name in ["is_int", "is_string", "get_debug_type", "array_values", "array_key_exists"] {
            assert!(!arg_reach(name).expect(name).reaches_blind(false), "{name}");
        }
    }

    #[test]
    fn an_unreadable_argument_list_reaches_what_strictness_leaves() {
        let strlen = arg_reach("strlen").expect("strlen");
        assert!(strlen.reaches_blind(false));
        assert!(!strlen.reaches_blind(true), "a coerced string parameter is a TypeError there");
        assert!(arg_reach("count").expect("count").reaches_blind(true));
    }

    /// The string family is certified at a call site only: no other pass knows
    /// it, and strictness alone rules out everything its arguments reach.
    #[test]
    fn the_string_family_reaches_only_through_coercion() {
        for name in [
            "strcmp", "strncmp", "strcasecmp", "strncasecmp", "strspn", "strcspn", "substr_count",
            "ord", "chr", "bin2hex", "hex2bin", "dirname", "unpack",
        ] {
            assert!(certified_at_call_site(name), "{name}");
            assert!(effect_labels(name).is_none() && !foldable(name), "{name} is known blind");
            assert!(!arg_reach(name).expect(name).reaches_blind(true), "{name}");
        }
        for name in ["basename", "strnatcasecmp", "substr_compare", "htmlspecialchars"] {
            assert!(!certified_at_call_site(name), "{name} reads the locale or an ini setting");
        }
    }
}
