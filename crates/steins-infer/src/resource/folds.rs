//! The folds that come with the kind (ADR-0097 §2.7): `gettype`,
//! `get_debug_type`, `get_resource_type` and `get_resource_id` over a handle the
//! walk has proven, answered from the handle's state and kind instead of the
//! declared `string`/`int`.
//!
//! The fifth fold of §2.7, `is_resource`, is a guard and lives with the other
//! type predicates (`predicates.rs`).
//!
//! # Where a fold is asked, and why only there
//!
//! The state is read on the store the statement started with, and the effects
//! of the statement's own calls land after it (`resource_call_effects`). That is
//! the state the handle is in when the fold's call runs only if **no other call
//! of the statement runs first**: in `$t = [fclose($h), gettype($h)]` the close
//! has already happened when `gettype` reads the handle, and a fold answering
//! `'resource'` there would be a wrong value, which a later comparison could
//! then use to call a live branch dead. So the rung is asked from two seams and
//! no others:
//!
//! * an assignment whose right-hand side **is** the call (`$t = gettype($h);`),
//!   the one statement shape where the call is all the statement runs;
//! * a dump whose argument is the call and whose other arguments run no code.
//!
//! A composed spelling — `gettype($h) . 'x'`, `(string) gettype($h)`, a ternary
//! arm, a condition — keeps the declared answer. It is two answers for one
//! value, and the weaker one is the sound one for a spelling that may have run
//! another call first.
//!
//! The key of an element place (`gettype($pipes[0])`, ADR-0098 §2.4) must be a
//! literal or a variable, for the same reason: `place_of` can resolve a key
//! through a project call, and at top level that call can rebind the base.

use std::collections::HashMap;

use steins_catalog::ResourceKind;
use steins_domain::{Base, Fact, IntRange, PhpStr, Refinement, Val};
use steins_syntax::ArgValue;

use crate::builtin_returns::transfer_envelope_admits;
use crate::cx::Cx;
use crate::env::{HandleState, HeapRes, Known, Store, Stratum};
use crate::fold::Folder;
use crate::offsets::place_of;
use crate::resource::proven_resource_state;

/// The four folds, by the builtin that asks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ResourceFold {
    GetType,
    DebugType,
    ResourceType,
    ResourceId,
}

impl ResourceFold {
    fn of(name: &str) -> Option<Self> {
        let name = name.trim_start_matches('\\');
        [
            ("gettype", ResourceFold::GetType),
            ("get_debug_type", ResourceFold::DebugType),
            ("get_resource_type", ResourceFold::ResourceType),
            ("get_resource_id", ResourceFold::ResourceId),
        ]
        .into_iter()
        .find(|(n, _)| name.eq_ignore_ascii_case(n))
        .map(|(_, f)| f)
    }

    /// The return type the engine must still declare for the rule to be the one
    /// written here (ADR-0061 §2's pin). Reflected at 8.5.10: `string` for the
    /// three spellings, `int` for the id.
    const fn declared(self) -> &'static [&'static str] {
        match self {
            ResourceFold::ResourceId => &["int"],
            _ => &["string"],
        }
    }
}

/// Every one of the four takes exactly one required parameter at 8.5.10
/// (`gettype(mixed $value)`, `get_debug_type(mixed $value)`,
/// `get_resource_type($resource)`, `get_resource_id($resource)`). The rule reads
/// argument 0, so a signature that moved is a rule gone stale.
const ARITY: Option<(u32, u32)> = Some((1, 1));

/// The **`get_resource_type()` spellings** a handle of this producer can answer
/// while open, by `(producer, kind)` — `get_debug_type()` wraps each in
/// `resource (…)`. Probed at 8.5.10 on a handle from every producer below.
///
/// These are PHP's spellings, not [`ResourceKind::as_str`]'s: a filter is
/// `stream filter` and a persistent stream `persistent stream`, with a space,
/// and a directory handle is a plain `stream`. `stream_socket_client` answers
/// either stream spelling, because `STREAM_CLIENT_PERSISTENT` makes it hand back
/// a persistent one under the same row. `pg_socket` has no row: it was not
/// probed, and a missing row declines the fold.
const KIND_SPELLINGS: &[(&str, ResourceKind, &[&str])] = &[
    ("bzopen", ResourceKind::Stream, &["stream"]),
    ("fopen", ResourceKind::Stream, &["stream"]),
    ("fsockopen", ResourceKind::Stream, &["stream"]),
    ("gzopen", ResourceKind::Stream, &["stream"]),
    ("opendir", ResourceKind::Dir, &["stream"]),
    ("pfsockopen", ResourceKind::PersistentStream, &["persistent stream"]),
    ("popen", ResourceKind::Stream, &["stream"]),
    ("proc_open", ResourceKind::Process, &["process"]),
    // `proc_open`'s pipes (ADR-0098 §2.2), minted under the producer's name.
    ("proc_open", ResourceKind::Stream, &["stream"]),
    ("socket_export_stream", ResourceKind::Stream, &["stream"]),
    ("stream_context_create", ResourceKind::StreamContext, &["stream-context"]),
    ("stream_context_get_default", ResourceKind::StreamContext, &["stream-context"]),
    ("stream_context_set_default", ResourceKind::StreamContext, &["stream-context"]),
    ("stream_filter_append", ResourceKind::StreamFilter, &["stream filter"]),
    ("stream_filter_prepend", ResourceKind::StreamFilter, &["stream filter"]),
    ("stream_socket_accept", ResourceKind::Stream, &["stream"]),
    ("stream_socket_client", ResourceKind::Stream, &["persistent stream", "stream"]),
    ("stream_socket_pair", ResourceKind::Stream, &["stream"]),
    ("stream_socket_server", ResourceKind::Stream, &["stream"]),
    ("tmpfile", ResourceKind::Stream, &["stream"]),
];

fn kind_spellings(res: &HeapRes) -> Option<&'static [&'static str]> {
    KIND_SPELLINGS
        .iter()
        .find(|(p, k, _)| *p == res.producer && *k == res.kind)
        .map(|(_, _, s)| *s)
}

/// What `gettype()` and `get_debug_type()` both say of a closed handle, and
/// `get_resource_type()` says instead of a kind (probed at 8.5.10, every
/// producer alike).
const CLOSED: &str = "resource (closed)";
const CLOSED_KIND: &str = "Unknown";

/// The fold of `name(args)` over a proven handle, or `None` — today's declared
/// answer stands — wherever the proof is missing.
///
/// Fires only where the argument judgments would read the same handle: a
/// variable or an element place whose contract lane is one `Verified` resource
/// arm ([`proven_resource_state`], the §8.6 lock), with the state that function
/// answers. The kind is read off the heap entry and has to have a probed
/// spelling.
///
/// | fold | `Open` | `Closed` | `Unknown` |
/// | --- | --- | --- | --- |
/// | `gettype` | `'resource'` | `'resource (closed)'` | both |
/// | `get_debug_type` | `'resource (<kind>)'` | `'resource (closed)'` | both |
/// | `get_resource_type` | `'<kind>'` | `'Unknown'` | both |
/// | `get_resource_id` | `int<1, max>` | `int<1, max>` | `int<1, max>` |
///
/// **No handle reaches the `Open` column today**: [`proven_resource_state`]
/// answers `Unknown` for every `Open`, because nothing makes that state durable
/// across a call that closes the handle without naming it (a bare `closedir()`,
/// an adopting `bzopen`/`socket_import_stream`). The column is the row a
/// re-established `Open` proof would take, and it is what keeps the `Unknown`
/// union honest: the union is exactly the two columns beside it. Everything the
/// folds buy over the declared `string` — the kind spelling, the closed
/// spelling, the two-member union a guard can still refute one half of — comes
/// from the other two columns.
///
/// The id is state-independent: a closed handle keeps its id (probed), and the
/// engine never hands out `0` — the first resource of a CLI request is `STDIN`,
/// id `1`, and a request with no standard streams (`php-cgi`) starts at `2`.
///
/// Gated by ADR-0061 §2 as every argument-reading rung is: the engine still
/// declares `string`/`int` with one parameter, no project function shadows the
/// name, and the answer is inside the declaration. The stratum is `Verified`:
/// the lane is, and the state is a heap proof.
pub(crate) fn resource_fold_return_fact(
    cx: &Cx,
    folder: &mut dyn Folder,
    name: &str,
    args: &[ArgValue],
    env: &HashMap<String, Known>,
    store: &Store,
    poisoned: bool,
) -> Option<(Fact, Stratum)> {
    if poisoned {
        return None;
    }
    let fold = ResourceFold::of(name)?;
    let [subject] = args else { return None };
    let place = subject_place(cx, folder, subject, env)?;
    let state = proven_resource_state(store, &place)?;
    let out = match fold {
        ResourceFold::ResourceId => {
            Fact::refined(Base::Int, Refinement::Int(IntRange::POSITIVE), false)
        }
        ResourceFold::GetType => strings(match state {
            HandleState::Open => vec!["resource".to_owned()],
            HandleState::Closed => vec![CLOSED.to_owned()],
            HandleState::Unknown => vec!["resource".to_owned(), CLOSED.to_owned()],
        })?,
        ResourceFold::DebugType | ResourceFold::ResourceType => {
            let closed = if fold == ResourceFold::DebugType { CLOSED } else { CLOSED_KIND };
            let mut members = Vec::new();
            if state != HandleState::Closed {
                let kinds = kind_spellings(store.res_of(&place)?)?;
                members.extend(kinds.iter().map(|k| match fold {
                    ResourceFold::DebugType => format!("resource ({k})"),
                    _ => (*k).to_owned(),
                }));
            }
            if state != HandleState::Open {
                members.push(closed.to_owned());
            }
            strings(members)?
        }
    };
    if !transfer_envelope_admits(cx, folder, name, fold.declared(), ARITY, &out) {
        return None;
    }
    Some((out, Stratum::Verified))
}

fn strings(members: Vec<String>) -> Option<Fact> {
    Fact::from_vals(members.into_iter().map(|s| Val::Str(PhpStr::from(s))).collect())
}

/// The place the fold's one argument names: a variable, or an element of one
/// under a key that is a literal or a variable the walk proved at `Verified`
/// (ADR-0098 §2.2's floor, via [`place_of`]). A key spelled any other way could
/// run code before the fold's call does, so it names nothing here.
fn subject_place(
    cx: &Cx,
    folder: &mut dyn Folder,
    subject: &ArgValue,
    env: &HashMap<String, Known>,
) -> Option<String> {
    match subject {
        ArgValue::Var(v) => Some(v.clone()),
        // Only the KEY is checked here: `place_of` already refuses a base that
        // is anything but a variable, so re-stating that would be a second copy
        // of an invariant with one owner.
        ArgValue::OffsetRead { key, .. }
            if key.is_literal() || matches!(**key, ArgValue::Var(_)) =>
        {
            place_of(cx, folder, subject, env, false)
        }
        _ => None,
    }
}

#[cfg(test)]
mod kind_spelling_table_tests {
    //! The shape of [`KIND_SPELLINGS`], which the integration suite cannot see.
    //! What each row ANSWERS is walked row by row over there
    //! (`every_row_of_the_spelling_table_folds_to_the_spelling_php_uses`, one
    //! fixture per row); what is pinned here is that the table has the rows that
    //! suite carries fixtures for, and that no row can shadow another.
    use super::KIND_SPELLINGS;

    /// One fixture per row lives in `tests/it/resource_folds.rs`. A row added
    /// here without one — or removed with one left behind — fails this.
    const ROWS: usize = 20;

    #[test]
    fn kind_spellings_covers_every_row() {
        assert_eq!(KIND_SPELLINGS.len(), ROWS, "add or remove the matching fixture row too");
    }

    #[test]
    fn no_producer_kind_pair_is_spelled_twice() {
        // `kind_spellings` answers with the FIRST match, so a duplicate key
        // would make one row unreachable and untestable. `proc_open` appears
        // twice on purpose — its process handle and its pipes — under two kinds.
        let mut keys: Vec<(&str, String)> =
            KIND_SPELLINGS.iter().map(|(p, k, _)| (*p, format!("{k:?}"))).collect();
        keys.sort_unstable();
        let before = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), before, "a `(producer, kind)` key appears twice");
    }

    #[test]
    fn every_row_carries_a_non_empty_spelling() {
        // An empty spelling list would fold `get_resource_type()` to the closed
        // singleton on an unknown state — a claim, not a decline.
        for (producer, _, spellings) in KIND_SPELLINGS {
            assert!(!spellings.is_empty(), "`{producer}` has no spelling");
            assert!(spellings.iter().all(|s| !s.is_empty()), "`{producer}` has an empty spelling");
        }
    }
}
