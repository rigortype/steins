//! PHP sidecar IPC — the resident helper process that runs the project's own
//! PHP to fold literal calls (ADR-0004, ADR-0024).
//!
//! # Two halves
//!
//! * [`wire`] — the **format**: [`FoldArg`]/[`FoldResult`]/[`EnvInfo`]/[`Reflection`]/[`PregCompile`]/[`ConstantDefined`],
//!   the `*_params` request constructors and the `parse_*_result` response parsers.
//!   Pure `serde_json`, so it compiles on `wasm32-unknown-unknown` too (ADR-0066).
//! * `process` — the **transport**: [`Sidecar`], a resident `php` child.
//!   `cfg(not(target_arch = "wasm32"))`, because no wasm runtime spawns a process.
//!
//! The split exists so a second transport (php-wasm in the browser, issue #64)
//! speaks the identical format by construction rather than by review.
//!
//! # Protocol
//!
//! JSON-RPC 2.0 with NDJSON framing over the child's stdin/stdout. The PHP side
//! is a single, dependency-free file (`runner.php`, embedded via [`include_str!`]),
//! launched as `php -r <source>` — the source itself as an argv element, never
//! written to disk. `php` is resolved from `PATH` at spawn time — the *project's
//! own* PHP, per ADR-0004.
//!
//! # Zero-FP contract (ADR-0024, binding)
//!
//! Sidecar misbehavior must NEVER become a wrong diagnostic. Every failure mode
//! — spawn failure, IO error, per-request timeout, malformed response, a child
//! that died outright — maps to [`FoldResult::Widen`], never a value. On any such
//! failure the child is killed and the instance is **poisoned**: the request in
//! flight is lost, and no half-dead process is ever trusted for an answer.
//!
//! Poison is a lost *answer*, not a lost run. A child can die uncatchably — an
//! allocation past `memory_limit` is a fatal no PHP `catch` can see — so the
//! *next* request replaces it with a fresh one, a bounded number of times per
//! instance ([`Sidecar`]). The request that killed the child is never retried on
//! the replacement.
//!
//! # Concurrency model
//!
//! No async runtime. A single background thread drains the child's stdout into a
//! channel; each request writes a line and waits on the channel with a timeout
//! ([`std::sync::mpsc::Receiver::recv_timeout`]). Requests are strictly
//! serialized (`&mut self`) and stateless, which is precisely what makes a
//! restart transparent: there is nothing to replay into the new child.

pub mod wire;

pub use wire::{
    ARRAY_TAG, ConstantDefined, EnvInfo, FoldArg, FoldKey, FoldResult, FoldValue, PregCompile,
    Reflection, defined_params, env_params, fold_arg_to_json, fold_params, parse_defined_result,
    parse_env_result, parse_fold_result, parse_fold_value, parse_preg_compile_result,
    parse_reflection_result, preg_compile_params, reflect_params,
};

#[cfg(not(target_arch = "wasm32"))]
mod process;

#[cfg(not(target_arch = "wasm32"))]
pub use process::Sidecar;
