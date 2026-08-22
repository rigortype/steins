//! The replay transport (ADR-0066): [`TableEngine`] answers from a supplied table
//! keyed by [`request_key`] and records the misses, so a fixture can pin exactly
//! what the sidecar was asked.

use std::collections::{HashMap, HashSet};

use steins_sidecar::{
    ClassReflection, ConstantDefined, EnvInfo, FoldArg, FoldResult, PregCompile, Reflection,
};

use crate::fold::{EngineFolder, FoldEngine};

// ---------------------------------------------------------------------------
// The replay transport: a supplied answer table, plus the misses (ADR-0066).
// ---------------------------------------------------------------------------

/// The canonical key of a sidecar request: the JSON-RPC request object **minus its
/// `id`**, serialized.
///
/// `id` is framing, not identity: two `fold(strtoupper, ["ab"])` requests differ in
/// `id` and ask the same question, and a memo table keyed on the whole request
/// would never hit. Everything else IS identity, and it comes from the wire
/// module's own `*_params` constructors, so a key built here and a request sent by
/// the process transport agree by construction.
///
/// The string is also the interchange format: the replay loop hands these keys out
/// as its pending list and takes them back as table keys, and each one parses as
/// `{"method": …, "params": …}` — everything a caller needs to answer it.
#[must_use]
pub fn request_key(method: &str, params: &serde_json::Value) -> String {
    serde_json::json!({ "method": method, "params": params }).to_string()
}

/// The replay [`FoldEngine`] (ADR-0066): answers from a supplied table of
/// already-known results, and records the questions it could not answer.
///
/// This is the transport that makes the sidecar surface reachable where no
/// process can be spawned — the browser (issue #64), where php-wasm answers
/// asynchronously and the analysis walk is synchronous. One run is one pass: a
/// miss declines *immediately* and is appended to [`Self::pending`], the caller
/// answers the pending set and runs again, and the answered set strictly grows,
/// so the fixpoint terminates. The iteration cap lives with the caller.
///
/// **A miss never fabricates.** It declines exactly as a dead sidecar declines,
/// which means a run with non-empty pending is a NoFold-grade run: sound, less
/// precise, and never to be shown to a user as if it were complete.
pub struct TableEngine {
    /// `request_key` → the raw JSON-RPC `result` value for that request.
    table: HashMap<String, serde_json::Value>,
    /// The misses, in order of first occurrence, deduped.
    pending: Vec<String>,
    /// Membership index for `pending`'s dedupe.
    asked: HashSet<String>,
}

impl TableEngine {
    /// A replay engine over `table` (`request_key` → raw `result` value). An empty
    /// table is the normal starting point: the first run answers nothing and
    /// reports every question the walk asked.
    #[must_use]
    pub fn new(table: HashMap<String, serde_json::Value>) -> Self {
        Self { table, pending: Vec::new(), asked: HashSet::new() }
    }

    /// The unanswered requests recorded so far, in first-occurrence order.
    #[must_use]
    pub fn pending(&self) -> &[String] {
        &self.pending
    }

    /// Take the unanswered requests, leaving the recorder empty.
    pub fn take_pending(&mut self) -> Vec<String> {
        self.asked.clear();
        std::mem::take(&mut self.pending)
    }

    /// Answer one request from the table, or record the miss and decline.
    fn ask(&mut self, method: &str, params: &serde_json::Value) -> Option<serde_json::Value> {
        let key = request_key(method, params);
        if let Some(answer) = self.table.get(&key) {
            return Some(answer.clone());
        }
        if self.asked.insert(key.clone()) {
            self.pending.push(key);
        }
        None
    }
}

impl FoldEngine for TableEngine {
    fn env(&mut self) -> Option<EnvInfo> {
        let answer = self.ask("env", &steins_sidecar::env_params())?;
        steins_sidecar::parse_env_result(&answer)
    }

    fn reflect(&mut self, target: &str) -> Option<Reflection> {
        let answer = self.ask("reflect", &steins_sidecar::reflect_params(target))?;
        steins_sidecar::parse_reflection_result(&answer, target)
    }

    fn reflect_class(&mut self, target: &str) -> Option<ClassReflection> {
        let answer = self.ask("reflect_class", &steins_sidecar::reflect_class_params(target))?;
        steins_sidecar::parse_class_reflection_result(&answer, target)
    }

    fn fold(&mut self, name: &str, args: &[FoldArg], strict: bool) -> FoldResult {
        // Not askable at all (a non-finite float has no JSON spelling), so it
        // is not a *pending* request either: recording it would put a key in
        // the table that no engine can ever answer.
        let Some(params) = steins_sidecar::fold_params(name, args, strict) else {
            return FoldResult::widen("unrepresentable argument");
        };
        match self.ask("fold", &params) {
            Some(answer) => steins_sidecar::parse_fold_result(&answer),
            // Unanswered: the same decline a dead sidecar gives.
            None => FoldResult::widen("pending"),
        }
    }

    fn preg_compile(&mut self, pattern: &str) -> Option<PregCompile> {
        let answer = self.ask("preg_compile", &steins_sidecar::preg_compile_params(pattern))?;
        steins_sidecar::parse_preg_compile_result(&answer)
    }

    fn constant_defined(&mut self, name: &str) -> Option<ConstantDefined> {
        let answer = self.ask("defined", &steins_sidecar::defined_params(name))?;
        steins_sidecar::parse_defined_result(&answer)
    }
}

/// The replay folder: the shared policy over the [`TableEngine`] transport.
///
/// # Replayability (ADR-0048)
///
/// A run of this folder is a **pure function of its table**. The walk is already
/// replayable — analysis is a function of (CST, canonical entry state, query
/// answers, fold memo) with no reliance on global ordering — and a `TableFolder`
/// adds nothing that could break that: it consults no clock, no process, no
/// filesystem, and no ambient state. The same source and the same table produce
/// the same findings and the same pending list, every time and on any target.
pub type TableFolder = EngineFolder<TableEngine>;

impl EngineFolder<TableEngine> {
    /// A fresh replay folder over `table`. Fresh per analysis run by construction:
    /// the memos inside are whole-run answers, and reusing them across tables
    /// would let a stale decline outlive the answer that fixes it.
    #[must_use]
    pub fn with_table(table: HashMap<String, serde_json::Value>) -> Self {
        Self::with_engine(TableEngine::new(table))
    }

    /// The unanswered requests this run recorded, in first-occurrence order.
    /// Non-empty ⇒ the run's results are NoFold-degraded and must not be shown.
    #[must_use]
    pub fn pending(&self) -> &[String] {
        self.engine.pending()
    }

    /// Take this run's unanswered requests, leaving the recorder empty.
    pub fn take_pending(&mut self) -> Vec<String> {
        self.engine.take_pending()
    }
}
