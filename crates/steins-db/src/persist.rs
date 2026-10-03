//! The per-package artifact payloads (ADR-0092 §2, issue #487): what the
//! `symbols`, `contracts` and `trace` sections of a package artifact mean,
//! the read transaction that loads them, and the residency-policy vocabulary.
//!
//! `steins-gen` owns the container and deliberately not the payloads ("what
//! section bytes *mean* belongs to the payload owners"); this crate owns the
//! shard type and consumes the trace IR, so the section names and codecs live
//! here. The inventory this slice pins:
//!
//! * [`SYMBOLS_SECTION`] — the [`PackageShard`] of issue #486, whole.
//! * [`CONTRACTS_SECTION`] — one [`DeclContract`] per function/method: the
//!   declaration's raw docblock and natively-declared signature, i.e. the
//!   *inputs* the declared-contract lane lowers, never the lowered form. The
//!   lowering (`envelopes_of` in `steins-infer`) resolves names against the
//!   project index at check time, so a stored lowered contract would either
//!   duplicate context the trace already carries or freeze a resolution that
//!   is not package-local; storing the inputs keeps warm and cold on the one
//!   lowering path — a cache miss may change cost, never meaning.
//! * [`TRACE_SECTION`] — the trace IR ([`SourceTree`]) in per-file shards
//!   behind a nested directory, so a binding descent into one vendor function
//!   decodes one file and a metadata query decodes none. Section names are 16
//!   bytes, so the per-file index nests *inside* the section, mirroring the
//!   container's own discipline one level down: a length-prefixed directory,
//!   then payloads that tile the rest of the section exactly.
//!
//! The `summaries` section is deliberately absent *here*: issue #489 owns that
//! schema, and it landed in `steins-infer` rather than beside these three,
//! because its payload is that crate's vocabulary end to end (`Diagnostic`,
//! the diagnostic-id registry, `Facet`, `Fix`) and this crate neither knows nor
//! should know any of it. Same discipline, one crate over — and the same line
//! the orchestrator's own `sources` section already draws.
//!
//! Codec: [`crate::wire`] inside every payload — a compact binary form of the
//! same serde schema, no new dependency. It replaced serde_json here in issue
//! #504, on the measurement that two thirds of the JSON encoding was field
//! names and punctuation the reader already knows; the swap was free by
//! construction, because the section boundary plus
//! [`steins_gen::SCHEMA_VERSION`] make an artifact of the previous schema an
//! ordinary [`Miss`] and one rebuild (a cache, no migration ever).
//!
//! The nested **directory** stays JSON, deliberately: it is framing rather
//! than payload, it is a fifth of a percent of the section, and every reader's
//! addressing is defined against it.
//!
//! Every failure path is a [`Miss`] the caller maps to rebuild-from-source:
//! absent sections, bytes that are not a value of the expected type, a
//! directory that lies about its offsets, a payload deeper than the decoder's
//! recursion ceiling. A *readable* payload that is semantically wrong (a shard
//! whose tables contradict each other) is not detectable here — the same
//! posture as the fold table's rows (ADR-0092 §4) — and is excluded by the
//! schema version plus the generation fingerprint, which no artifact is read
//! without.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use steins_gen::{
    ArtifactBuilder, ArtifactReader, Generation, Miss, PackageKind, PackageName, SectionName,
};
use steins_syntax::{NativeType, NsCtx, Param, SourceTree, Span};

use crate::shard::PackageShard;

// ---------------------------------------------------------------------------
// The section inventory.
// ---------------------------------------------------------------------------

/// The section holding the package's [`PackageShard`], serialized whole.
pub const SYMBOLS_SECTION: &str = "symbols";

/// The section holding the package's declared-contract inputs: a JSON array
/// of [`DeclContract`], in file-slot then declaration order.
pub const CONTRACTS_SECTION: &str = "contracts";

/// The section holding the package's trace IR in per-file shards: a
/// length-prefixed directory ([`TraceIndex`]), then one serialized
/// [`SourceTree`] per file.
pub const TRACE_SECTION: &str = "trace";

/// The section holding the package's per-file **facts** (issue #516): the
/// projection of a file's tree that every whole-universe phase reads, so a
/// warm run answers those phases without decoding a tree. Framed exactly like
/// [`TRACE_SECTION`] — same nested directory, same per-file addressing, same
/// byte-copy discipline on republish — because it is asked the same way: one
/// file at a time, and never all of them at once for a package that did not
/// move. The payload's *meaning* belongs to `steins-infer`, which owns every
/// vocabulary in it; this crate owns only the framing.
pub const FACTS_SECTION: &str = "facts";

/// [`SYMBOLS_SECTION`] as a validated [`SectionName`].
#[must_use]
pub fn symbols_section() -> SectionName {
    SectionName::new(SYMBOLS_SECTION).expect("the symbols section name is valid")
}

/// [`CONTRACTS_SECTION`] as a validated [`SectionName`].
#[must_use]
pub fn contracts_section() -> SectionName {
    SectionName::new(CONTRACTS_SECTION).expect("the contracts section name is valid")
}

/// [`TRACE_SECTION`] as a validated [`SectionName`].
#[must_use]
pub fn trace_section() -> SectionName {
    SectionName::new(TRACE_SECTION).expect("the trace section name is valid")
}

/// [`FACTS_SECTION`] as a validated [`SectionName`].
#[must_use]
pub fn facts_section() -> SectionName {
    SectionName::new(FACTS_SECTION).expect("the facts section name is valid")
}

/// The serde codec for [`steins_phpdoc::MagicTagKind`], by its canonical
/// `label()` spelling. Lives here rather than in `steins-phpdoc` so the
/// zero-dep parser crate (ADR-0024) stays dependency-free; the inverse is
/// strict — a spelling outside the closed set is a decode error, which the
/// section reader degrades to a [`Miss`].
pub(crate) mod magic_tag_kind {
    use steins_phpdoc::MagicTagKind;

    const ALL: [MagicTagKind; 7] = [
        MagicTagKind::Method,
        MagicTagKind::Property,
        MagicTagKind::PropertyRead,
        MagicTagKind::PropertyWrite,
        MagicTagKind::Mixin,
        MagicTagKind::TypeAlias,
        MagicTagKind::ImportedTypeAlias,
    ];

    pub(crate) fn serialize<S: serde::Serializer>(
        v: &MagicTagKind,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(v.label())
    }

    pub(crate) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<MagicTagKind, D::Error> {
        let spelled = <std::borrow::Cow<'_, str> as serde::Deserialize>::deserialize(deserializer)?;
        ALL.into_iter().find(|k| k.label() == spelled).ok_or_else(|| {
            serde::de::Error::custom(format!("unknown magic-member tag {spelled:?}"))
        })
    }
}

// ---------------------------------------------------------------------------
// The contracts payload: declared-contract inputs, per declaration.
// ---------------------------------------------------------------------------

/// Which declaration a [`DeclContract`] belongs to, within its file's own
/// declaration lists — the same indices a [`crate::ShardSite`] carries, so a
/// symbol resolution and a contract lookup name one declaration one way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ContractOwner {
    /// `SourceTree::functions()[index]`.
    Function { index: usize },
    /// `SourceTree::classes()[class].methods[method]`.
    Method { class: usize, method: usize },
}

/// One declaration's declared-contract *inputs* (the raw docblock and the
/// natively-declared signature), extracted per function and per method so a
/// contract question reads this section and never decodes a trace shard. The
/// module docs state why the inputs are stored rather than the lowered
/// `ContractTy`: the lowering is context-dependent and stays on one code path
/// for warm and cold alike.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclContract {
    /// The owning file's universe slot — [`crate::ShardSite::file`]'s space.
    pub file: usize,
    /// Which declaration in that file.
    pub owner: ContractOwner,
    /// The function's own FQN, or the declaring class-like's FQN for a
    /// method — lowercase-normalized, the index's key space.
    pub fqn: String,
    /// The simple name as written (method name for methods).
    pub name: String,
    /// The declaration name's span, for a consumer that positions on it.
    pub span: Span,
    /// The raw `/** … */` docblock adopted by the declaration, verbatim.
    pub docblock: Option<String>,
    /// The declared parameters — names (the `@param` join key), native
    /// types, defaults, spans — exactly as lowered.
    pub params: Vec<Param>,
    /// The declared native return type, where one lowers.
    pub ret: Option<NativeType>,
    /// The namespace context enclosing the declaration, so docblock class
    /// references resolve without the file's tree in hand.
    pub ctx: NsCtx,
}

/// Extract every declaration's contract inputs from one file's tree, in
/// declaration order — the projection the builder writes into
/// [`CONTRACTS_SECTION`]. Closures are deliberately absent: they have no
/// declaration identity outside their scope, and their docblocks ride the
/// trace shard.
#[must_use]
pub fn decl_contracts(slot: usize, tree: &SourceTree) -> Vec<DeclContract> {
    let mut out = Vec::new();
    for (index, f) in tree.functions().iter().enumerate() {
        out.push(DeclContract {
            file: slot,
            owner: ContractOwner::Function { index },
            fqn: f.fqn.clone(),
            name: f.name.clone(),
            span: f.span,
            docblock: f.docblock.clone(),
            params: f.params.clone(),
            ret: f.ret.clone(),
            ctx: tree.ctx_at(f.span.start).clone(),
        });
    }
    for (class, c) in tree.classes().iter().enumerate() {
        for (method, m) in c.methods.iter().enumerate() {
            out.push(DeclContract {
                file: slot,
                owner: ContractOwner::Method { class, method },
                fqn: c.fqn.clone(),
                name: m.name.clone(),
                span: m.span,
                docblock: m.docblock.clone(),
                params: m.params.clone(),
                ret: m.ret.clone(),
                ctx: tree.ctx_at(m.span.start).clone(),
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The trace payload: per-file shards behind a nested directory.
// ---------------------------------------------------------------------------

/// The byte width of the directory-length prefix at the head of the trace
/// section (a little-endian `u64`).
const TRACE_PREFIX: u64 = 8;

/// One file's entry in the trace section's nested directory. `offset` is
/// relative to the start of the payload area (the byte after the directory),
/// and entries tile that area exactly — the same strictness the container
/// applies to sections, one level down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TraceEntry {
    /// The file's diagnostic path — the lookup key.
    path: String,
    /// The file's universe slot, matching the shard's sites.
    slot: usize,
    /// Byte offset of the file's payload within the payload area.
    offset: u64,
    /// Byte length of the file's payload.
    len: u64,
}

/// One file of the trace payload, on its way in: the diagnostic path, the
/// universe slot, and the lowered tree.
#[derive(Clone, Copy)]
pub struct TraceFile<'a> {
    pub path: &'a str,
    pub slot: usize,
    pub tree: &'a SourceTree,
}

/// Frame already-serialized per-file payloads into one section's bytes: a
/// length-prefixed JSON directory, then the payloads tiling the rest exactly.
///
/// Three sections share this framing — `trace`, `contracts` and `facts` — and
/// they share it for one reason: a warm rebuild must be able to *copy* an
/// unmoved file's payload byte for byte instead of re-encoding it (issue
/// #516). Re-encoding needs the value, the value needs the tree, and needing
/// the tree is exactly what the warm path is trying to stop doing. The slot
/// lives in the directory rather than in the payload, so a file whose universe
/// slot moved republishes the same bytes under a new directory entry.
#[must_use]
pub fn payload_section_bytes(parts: &[(String, usize, Vec<u8>)]) -> Vec<u8> {
    let mut entries = Vec::with_capacity(parts.len());
    let mut offset = 0u64;
    for (path, slot, payload) in parts {
        entries.push(TraceEntry {
            path: path.clone(),
            slot: *slot,
            offset,
            len: payload.len() as u64,
        });
        offset += payload.len() as u64;
    }
    let directory = serde_json::to_vec(&entries).expect("a payload directory serializes");
    let mut bytes =
        Vec::with_capacity(TRACE_PREFIX as usize + directory.len() + offset as usize);
    bytes.extend_from_slice(&(directory.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&directory);
    for (_, _, payload) in parts {
        bytes.extend_from_slice(payload);
    }
    bytes
}

/// Serialize the trace section: the nested directory, then one
/// [`SourceTree`] payload per file, in the order given. Paths must be
/// distinct — the reader refuses a directory with a duplicate, so a builder
/// that passes one has already lost.
#[must_use]
pub fn trace_section_bytes(files: &[TraceFile<'_>]) -> Vec<u8> {
    let parts: Vec<(String, usize, Vec<u8>)> = files
        .iter()
        .map(|f| {
            let payload = trace_payload(f.tree);
            (f.path.to_owned(), f.slot, payload)
        })
        .collect();
    payload_section_bytes(&parts)
}

/// One section's decoded per-file directory: which files it carries and where
/// each payload sits. Opening it reads and validates the directory only — no
/// payload is touched until [`Self::payload`] asks for one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadIndex {
    section: SectionName,
    entries: Vec<TraceEntry>,
    /// The directory's on-disk length, as the section's own prefix declared
    /// it — the payload area starts right after. Kept rather than recomputed:
    /// a republish copies every unmoved payload, so re-serializing the whole
    /// directory once per copy made the copy quadratic in the package.
    directory_len: u64,
}

impl PayloadIndex {
    /// Read and validate the nested directory. Strict, mirroring the
    /// container's own directory checks: the length prefix must fit the
    /// section, the directory must be exactly the declared JSON shape, and
    /// the payloads must tile the payload area exactly. Anything else is a
    /// [`Miss`].
    pub fn open(reader: &mut ArtifactReader, section: SectionName) -> Result<Self, Miss> {
        let section_len =
            reader.section_len(&section).ok_or_else(|| Miss::AbsentSection(section.clone()))?;
        if section_len < TRACE_PREFIX {
            return Err(Miss::Corrupt("payload section shorter than its length prefix"));
        }
        let prefix = reader.section_slice(&section, 0, TRACE_PREFIX)?;
        let dir_len = u64::from_le_bytes(prefix.try_into().expect("eight bytes read"));
        if dir_len > section_len - TRACE_PREFIX {
            return Err(Miss::Corrupt("payload directory longer than its section"));
        }
        let directory = reader.section_slice(&section, TRACE_PREFIX, dir_len)?;
        let entries: Vec<TraceEntry> = serde_json::from_slice(&directory)
            .map_err(|_| Miss::Corrupt("payload directory is not a directory"))?;
        let payload_area = section_len - TRACE_PREFIX - dir_len;
        let mut expected = 0u64;
        for entry in &entries {
            if entry.offset != expected {
                return Err(Miss::Corrupt("payloads do not tile the payload area"));
            }
            expected = entry
                .offset
                .checked_add(entry.len)
                .ok_or(Miss::Corrupt("payload length overflows"))?;
            if entries.iter().filter(|e| e.path == entry.path).count() != 1 {
                return Err(Miss::Corrupt("duplicate path in the payload directory"));
            }
        }
        if expected != payload_area {
            return Err(Miss::Corrupt("payloads do not tile the payload area"));
        }
        Ok(Self { section, entries, directory_len: dir_len })
    }

    /// Every file in the directory, `(path, slot)`, in on-disk order.
    pub fn files(&self) -> impl Iterator<Item = (&str, usize)> {
        self.entries.iter().map(|e| (e.path.as_str(), e.slot))
    }

    /// Whether the directory lists `path`.
    #[must_use]
    pub fn has_file(&self, path: &str) -> bool {
        self.entries.iter().any(|e| e.path == path)
    }

    /// One file's payload bytes, verbatim: one bounded sub-range read of
    /// exactly its payload — the other files' bytes are never touched. This is
    /// also the republish path's copy source (issue #516).
    pub fn payload(&self, reader: &mut ArtifactReader, path: &str) -> Result<Vec<u8>, Miss> {
        let entry = self
            .entries
            .iter()
            .find(|e| e.path == path)
            .ok_or(Miss::Corrupt("no payload entry for the requested path"))?;
        reader.section_slice(
            &self.section,
            TRACE_PREFIX + self.directory_len + entry.offset,
            entry.len,
        )
    }
}

/// The trace section's decoded directory — [`PayloadIndex`] over
/// [`TRACE_SECTION`], with the tree decode on top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceIndex {
    inner: PayloadIndex,
}

impl TraceIndex {
    /// Read and validate the trace section's nested directory.
    pub fn open(reader: &mut ArtifactReader) -> Result<Self, Miss> {
        Ok(Self { inner: PayloadIndex::open(reader, trace_section())? })
    }

    /// Every file in the directory, `(path, slot)`, in on-disk order.
    pub fn files(&self) -> impl Iterator<Item = (&str, usize)> {
        self.inner.files()
    }

    /// Whether the directory lists `path`.
    #[must_use]
    pub fn has_file(&self, path: &str) -> bool {
        self.inner.has_file(path)
    }

    /// One file's payload bytes, verbatim — the republish copy source.
    pub fn payload(&self, reader: &mut ArtifactReader, path: &str) -> Result<Vec<u8>, Miss> {
        self.inner.payload(reader, path)
    }

    /// Load one file's tree: one bounded sub-range read of exactly its
    /// payload, one decode — the other files' bytes are never touched. An
    /// unlisted path, or a payload that does not decode to a tree, is a
    /// [`Miss`] for this file alone; the directory (and every other payload)
    /// still serves.
    pub fn read_tree(&self, reader: &mut ArtifactReader, path: &str) -> Result<SourceTree, Miss> {
        let bytes = self.inner.payload(reader, path)?;
        crate::wire::from_slice(&bytes).map_err(|_| Miss::Corrupt("trace payload is not a tree"))
    }
}

// ---------------------------------------------------------------------------
// Assembling and reading a package artifact.
// ---------------------------------------------------------------------------

/// Assemble one package's three sections into a container builder, ready for
/// [`steins_gen::Candidate::write_artifact`] (or [`ArtifactBuilder::write_to`]
/// in tests and tools). The builder wiring that decides *which* packages to
/// build is issue #489/#491's; this is only what one package's payload is.
#[must_use]
pub fn build_sections(
    shard: &PackageShard,
    contracts: &[(String, usize, Vec<u8>)],
    trace: &[(String, usize, Vec<u8>)],
) -> ArtifactBuilder {
    let mut builder = ArtifactBuilder::new();
    let symbols = crate::wire::to_vec(shard).expect("a package shard serializes");
    builder.section(symbols_section(), symbols).expect("distinct section names");
    builder
        .section(contracts_section(), payload_section_bytes(contracts))
        .expect("distinct section names");
    builder
        .section(trace_section(), payload_section_bytes(trace))
        .expect("distinct section names");
    builder
}

/// One file's trace payload, ready for [`build_sections`]: the lowered tree,
/// serialized. The slot lives in the directory, so these bytes are a function
/// of the file alone and republish can copy them under a new slot.
#[must_use]
pub fn trace_payload(tree: &SourceTree) -> Vec<u8> {
    // Infallible for the lowered representation: every sequence and map has a
    // known length and floats travel as bits (see steins-syntax's `persist`
    // codecs), so nothing the codec refuses can arrive.
    crate::wire::to_vec(tree).expect("a lowered tree serializes")
}

/// One file's contract payload, ready for [`build_sections`]: the file's
/// declarations at the **canonical slot 0**, so the bytes are a function of
/// the file alone and republish can copy them whatever slot the file now
/// holds. [`read_contracts`] puts the directory's slot back.
#[must_use]
pub fn contract_payload(tree: &SourceTree) -> Vec<u8> {
    crate::wire::to_vec(&decl_contracts(0, tree)).expect("a contract list serializes")
}

/// Decode the `symbols` section back into the [`PackageShard`] it was
/// serialized from. Any way the bytes can be wrong is a [`Miss`].
pub fn read_shard(reader: &mut ArtifactReader) -> Result<PackageShard, Miss> {
    let bytes = reader.section(&symbols_section())?;
    crate::wire::from_slice(&bytes).map_err(|_| Miss::Corrupt("symbols section is not a shard"))
}

/// Decode the `contracts` section back into its [`DeclContract`] list. Any
/// way the bytes can be wrong is a [`Miss`].
pub fn read_contracts(reader: &mut ArtifactReader) -> Result<Vec<DeclContract>, Miss> {
    let index = PayloadIndex::open(reader, contracts_section())?;
    let files: Vec<(String, usize)> =
        index.files().map(|(path, slot)| (path.to_owned(), slot)).collect();
    let mut out = Vec::new();
    for (path, slot) in files {
        let bytes = index.payload(reader, &path)?;
        let decls: Vec<DeclContract> = crate::wire::from_slice(&bytes)
            .map_err(|_| Miss::Corrupt("contracts payload is not a contract list"))?;
        // The payload is written at the canonical slot 0 so its bytes are a
        // function of the file alone; the directory is what says where the
        // file sits in *this* universe.
        out.extend(decls.into_iter().map(|mut d| {
            d.file = slot;
            d
        }));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Residency: the read transaction and the policy vocabulary.
// ---------------------------------------------------------------------------

/// Where a package's loaded payloads may stay resident (ADR-0092 §2:
/// residency is a policy axis independent of persistence). Two values, plain
/// data, no machinery: the type exists so the builder slices (#489/#491) can
/// fold the chosen policy into the generation fingerprint — a policy change
/// invalidates deliberately — and so the intended default is spellable now.
/// Nothing in this slice acts on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResidencyPolicy {
    /// Keep the package's payloads in memory for the life of the analysis:
    /// the first-party posture — these are the packages every query touches.
    FirstPartyResident,
    /// Load on demand through a [`ReadTxn`] and drop with it: the vendor
    /// posture — a binding descent (depth ≤ 8) reads one file's trace shard
    /// and lets it go.
    VendorOffloadable,
}

impl ResidencyPolicy {
    /// The intended default per package kind: first-party posture resident,
    /// everything else offloadable.
    #[must_use]
    pub fn default_for(kind: PackageKind) -> Self {
        if kind.is_first_party() {
            ResidencyPolicy::FirstPartyResident
        } else {
            ResidencyPolicy::VendorOffloadable
        }
    }

    /// A stable spelling, for the fingerprint field the builder slices add.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ResidencyPolicy::FirstPartyResident => "first-party-resident",
            ResidencyPolicy::VendorOffloadable => "vendor-offloadable",
        }
    }
}

/// One reading operation's view of one package artifact: owns the open
/// reader and everything it decoded, and frees the lot on drop — no global
/// residency state, no LRU, no cleverness (ADR-0092 §2, "loaded shards are
/// owned by the reading operation and dropped with it").
///
/// Loading is lazy and per part: [`ReadTxn::shard`] decodes the symbols
/// section once, [`ReadTxn::tree`] opens the trace directory once and then
/// decodes exactly the asked-for file, caching it for the transaction's
/// lifetime so a descent that revisits a file pays once. Every decode
/// failure surfaces as the [`Miss`] it is; nothing is cached but successes,
/// so a caller that maps a miss to rebuild never sees a partial value.
pub struct ReadTxn {
    reader: ArtifactReader,
    shard: Option<PackageShard>,
    contracts: Option<Vec<DeclContract>>,
    trace: Option<TraceIndex>,
    trees: BTreeMap<String, SourceTree>,
}

impl ReadTxn {
    /// Open a transaction over one package of a published generation.
    pub fn open(generation: &Generation, package: &PackageName) -> Result<Self, Miss> {
        Ok(Self::from_reader(generation.artifact(package)?))
    }

    /// A transaction over an already-open artifact (tests, tools).
    #[must_use]
    pub fn from_reader(reader: ArtifactReader) -> Self {
        Self {
            reader,
            shard: None,
            contracts: None,
            trace: None,
            trees: BTreeMap::new(),
        }
    }

    /// The package's symbol shard, decoded on first ask.
    pub fn shard(&mut self) -> Result<&PackageShard, Miss> {
        if self.shard.is_none() {
            self.shard = Some(read_shard(&mut self.reader)?);
        }
        Ok(self.shard.as_ref().expect("just decoded"))
    }

    /// The package's declared-contract inputs, decoded on first ask.
    pub fn contracts(&mut self) -> Result<&[DeclContract], Miss> {
        if self.contracts.is_none() {
            self.contracts = Some(read_contracts(&mut self.reader)?);
        }
        Ok(self.contracts.as_deref().expect("just decoded"))
    }

    /// The trace directory, opened on first ask — still no payload decoded.
    pub fn trace(&mut self) -> Result<&TraceIndex, Miss> {
        Self::ensure_trace(&mut self.reader, &mut self.trace)
    }

    /// One file's tree, decoded on first ask and owned by this transaction.
    pub fn tree(&mut self, path: &str) -> Result<&SourceTree, Miss> {
        if !self.trees.contains_key(path) {
            let index = Self::ensure_trace(&mut self.reader, &mut self.trace)?;
            let tree = index.read_tree(&mut self.reader, path)?;
            self.trees.insert(path.to_owned(), tree);
        }
        Ok(self.trees.get(path).expect("just inserted"))
    }

    /// How many file trees this transaction currently holds — the
    /// observability hook the laziness tests read.
    #[must_use]
    pub fn resident_trees(&self) -> usize {
        self.trees.len()
    }

    fn ensure_trace<'a>(
        reader: &mut ArtifactReader,
        slot: &'a mut Option<TraceIndex>,
    ) -> Result<&'a TraceIndex, Miss> {
        if slot.is_none() {
            *slot = Some(TraceIndex::open(reader)?);
        }
        Ok(slot.as_ref().expect("just opened"))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use steins_gen::{DecodeBudget, EnginePosture, GenerationInputs, Store};
    use steins_syntax::{
        ArgValue, OperatorConstruct, OperatorFamily, SiteKind, SiteOrigin, Stmt, StmtKind,
    };

    use super::*;
    use crate::shard::{fallback_package_key, merge_shards};
    use crate::{Project, ProjectIndex, SourceFile, SteinsDatabase, project_index};

    /// A throwaway directory under the OS temp dir, cleaned on drop.
    struct TempDir {
        dir: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "steins-db-persist-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A fixture wide enough that every payload leg is observable: multiple
    /// packages under the fallback grouping, docblocked functions and methods
    /// (contract inputs), namespaced declarations with `use` imports (the
    /// `ctx` leg), value-IR corners the codec exceptions exist for (a
    /// non-finite float literal, a non-UTF-8 string literal), construct
    /// keywords (`echo`, `exit`, `require_once`), the trace-IR constructs
    /// [`TRACE_CONSTRUCTS`] names (casts, interpolation, offset writes, the
    /// structured loops) beside `if` and closures, magic-member tags, alias
    /// edges, constants, property writes, and state constructs (`static`, a
    /// superglobal). A new trace-IR construct is one more line of PHP here, next
    /// to its family, and one more row there.
    fn fixture() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "src/app.php",
                "<?php\nnamespace App;\nuse Lib\\A\\Widget;\n/** @param int $n @return string */\nfunction run(int $n): string {\n  $f = 1e999;\n  $g = 2.5;\n  $s = \"\\xC0\\xC1\";\n  $i = (int) $g;\n  $t = \"x $s\";\n  $a = [1 => 'one', 'k' => $s];\n  $a[] = $t;\n  $a['k'] = $i;\n  unset($a['k']);\n  if ($n === 1) { echo $s; } else { exit; }\n  while ($n > 0) { $n--; }\n  for ($j = 0, $w = 'abc'; $j < $n; $j++) { $b[] = $w; }\n  foreach ($a as $k => $v) { $b[] = $v; }\n  foreach ($b as &$r) { $r = 1; }\n  foreach ($a[1] as [$p, $q]) { $b[] = $p; }\n  do { $n--; } while ($n > 0);\n  $c = fn (int $x): int => $x + 1;\n  return dup((string) $n);\n}\nconst LIMIT = 3;\n$k->written = 1;\nclass_alias('lib\\\\a\\\\widget', 'app\\\\widget');\n",
            ),
            (
                "src/dynamic.php",
                "<?php\nnamespace App;\nfunction load(string $c, string $f): mixed {\n  eval($c);\n  return require_once $f;\n}\n",
            ),
            (
                "vendor/lib/a/src/widget.php",
                "<?php\nnamespace Lib\\A;\n/** @method int magic() */\nclass Widget {\n  /** @param string $s @return int */\n  public function m(string $s = \"d\"): int { return \\strlen($s); }\n  public static function n(): self { return new self(); }\n  public function rows(): array { return [1]; }\n  public function bag(): object { return new self(); }\n}\nfunction dup(string $x): string { return $x; }\n",
            ),
            (
                "vendor/lib/b/src/dup.php",
                "<?php\nnamespace Lib\\A;\nfunction dup(string $x): string { return $x . 'b'; }\n/** @mixin Widget */\nclass Same {}\ndefine('FLAG', true);\n$o->{$name} = 5;\n",
            ),
            (
                "vendor/lib/b/src/origins.php",
                "<?php\nnamespace Lib\\A;\nclass Origins {\n  public function each(array $a, $obj, $dyn): void {\n    $this->each($a, $obj, $dyn);\n    $obj->$dyn();\n    \\array_map('strlen', $a);\n    $f = static function (): int { return 1; };\n    $f();\n    $g = function (): iterable { return []; };\n    $g();\n    try { throw new \\RuntimeException(); } catch (\\Exception $e) { throw $e; }\n  }\n}\n",
            ),
            (
                "vendor/lib/b/src/state.php",
                "<?php\nnamespace Lib\\A;\nfunction tally(): int {\n  static $n = 0;\n  return ++$n + \\count($_GET);\n}\n",
            ),
            (
                "vendor/lib/b/src/operators.php",
                "<?php\nnamespace Lib\\A;\nclass Operators {\n  public function each($o, $b): \\Generator {\n    $s = $o . $b;\n    $s .= $o;\n    $s = \"x{$o}\";\n    $s = <<<T\n      v {$o}\n      T;\n    $s = (string) $o;\n    echo $o;\n    print $o;\n    $t = $o == 'a';\n    $t = $o < $b;\n    switch ($o) { case 'a': break; }\n    $v = $o->p;\n    $o->p = 1;\n    $o->p++;\n    $t = isset($o->p);\n    $t = empty($o->p);\n    unset($o->p);\n    $t = $o->p ?? 1;\n    $o->p ??= 1;\n    $r = &$o->p;\n    [$x, $y] = $o;\n    foreach ($o as $z) {}\n    yield from $o;\n    f(...$o);\n    $c = clone $o;\n    $w = $this->q;\n    $d = clone($o, ['a' => 1]);\n    $m = $o->$b;\n    $n = $$b;\n    $o->p[0] = $b;\n    $q = new Q;\n    $q = null;\n    unset($q);\n    new Q;\n  }\n}\n",
            ),
            ("vendor/autoload.php", "<?php\nfunction stray_helper() {}\n"),
        ]
    }

    /// Every site the parsed fixture carries: functions', methods' and closure
    /// scopes'.
    fn fixture_sites<'a>(parsed: &'a [(&'static str, SourceTree)]) -> Vec<&'a SiteOrigin> {
        let mut sites = Vec::new();
        for (_, tree) in parsed {
            for f in tree.functions() {
                sites.extend(&f.sites);
            }
            for c in tree.classes() {
                for m in &c.methods {
                    sites.extend(&m.sites);
                }
            }
            for s in tree.scopes() {
                sites.extend(&s.sites);
            }
        }
        sites
    }

    /// Whether a lowered statement is one [`TRACE_CONSTRUCTS`] row's construct.
    type IsConstruct = fn(&StmtKind) -> bool;

    /// The trace-IR constructs the fixture must carry for the whole-tree
    /// equality in [`a_package_round_trips_through_its_artifact`] to test their
    /// wire shape: each is a variant, or a variant's field, a trace-IR PR added.
    /// Which PHP lowers to which shape is steins-syntax's to pin; this only
    /// proves the fixture has one of each.
    const TRACE_CONSTRUCTS: &[(&str, IsConstruct)] = &[
        ("a value-position cast", |k| {
            matches!(k, StmtKind::Assign { value: ArgValue::Cast { .. }, .. })
        }),
        ("an interpolated string's seeded concat chain", |k| {
            matches!(k, StmtKind::Assign { value: ArgValue::Concat(head, _), .. }
                if matches!(**head, ArgValue::Concat(..)))
        }),
        ("an auto-index append", |k| matches!(k, StmtKind::OffsetAppend { .. })),
        ("a keyed offset write", |k| matches!(k, StmtKind::OffsetWrite { .. })),
        ("an offset unset", |k| matches!(k, StmtKind::OffsetUnset { .. })),
        ("a while with a body", |k| {
            matches!(k, StmtKind::While { body, .. } if !body.is_empty())
        }),
        ("a for with init, body and a carried name", |k| {
            matches!(k, StmtKind::For { init, body, carried, .. }
                if !init.is_empty() && !body.is_empty() && !carried.is_empty())
        }),
        ("a foreach with a keyed header and a body", |k| {
            matches!(k, StmtKind::Foreach {
                subject: Some(_), key_var: Some(_), value_var: Some(_), by_ref: false, body, ..
            } if !body.is_empty())
        }),
        ("a by-reference foreach", |k| matches!(k, StmtKind::Foreach { by_ref: true, .. })),
        ("a foreach over an expression into a destructuring target", |k| {
            matches!(k, StmtKind::Foreach { subject: None, value_var: None, .. })
        }),
        ("a do-while with a body", |k| {
            matches!(k, StmtKind::DoWhile { body, .. } if !body.is_empty())
        }),
    ];

    /// Every statement kind in the parsed fixture's scopes, nested bodies
    /// included.
    fn fixture_stmt_kinds<'a>(parsed: &'a [(&'static str, SourceTree)]) -> Vec<&'a StmtKind> {
        fn walk<'a>(stmts: &'a [Stmt], out: &mut Vec<&'a StmtKind>) {
            for s in stmts {
                out.push(&s.kind);
                match &s.kind {
                    StmtKind::If { then_trace, elseifs, else_trace, .. } => {
                        walk(then_trace, out);
                        elseifs.iter().for_each(|(_, b)| walk(b, out));
                        walk(else_trace.as_deref().unwrap_or_default(), out);
                    }
                    StmtKind::For { init, body, .. } => {
                        walk(init, out);
                        walk(body, out);
                    }
                    StmtKind::While { body, .. }
                    | StmtKind::Foreach { body, .. }
                    | StmtKind::DoWhile { body, .. } => walk(body, out),
                    _ => {}
                }
            }
        }
        let mut kinds = Vec::new();
        for (_, tree) in parsed {
            for sc in tree.scopes() {
                walk(&sc.stmts, &mut kinds);
            }
        }
        kinds
    }

    /// The variant names serde decodes `T` by, in index order, which is what
    /// a variant index on the payload codec's wire means. A derived
    /// `Deserialize` hands the list to `deserialize_enum`; this deserializer
    /// records it there and refuses everything else.
    fn serde_variants<T: serde::de::DeserializeOwned>() -> &'static [&'static str] {
        struct Variants(Option<&'static [&'static str]>);

        impl<'de> serde::Deserializer<'de> for &mut Variants {
            type Error = serde::de::value::Error;

            fn deserialize_any<V: serde::de::Visitor<'de>>(
                self,
                _visitor: V,
            ) -> Result<V::Value, Self::Error> {
                Err(serde::de::Error::custom("not an enum"))
            }

            fn deserialize_enum<V: serde::de::Visitor<'de>>(
                self,
                _name: &'static str,
                variants: &'static [&'static str],
                _visitor: V,
            ) -> Result<V::Value, Self::Error> {
                self.0 = Some(variants);
                Err(serde::de::Error::custom("the variant list is all this reads"))
            }

            serde::forward_to_deserialize_any! {
                bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes
                byte_buf option unit unit_struct newtype_struct seq tuple tuple_struct map
                struct identifier ignored_any
            }
        }

        let mut variants = Variants(None);
        assert!(T::deserialize(&mut variants).is_err(), "the recorder decodes nothing");
        variants.0.expect("an enum's `Deserialize` names its variants")
    }

    /// Parse the fixture and group it exactly as the production constructors
    /// do: `fallback_package_key` over the diagnostic path, universe slots in
    /// file order.
    fn parsed_fixture() -> Vec<(&'static str, SourceTree)> {
        fixture().iter().map(|&(p, s)| (p, SourceTree::parse(s))).collect()
    }

    struct BuiltPackage {
        key: String,
        shard: PackageShard,
        contracts: Vec<DeclContract>,
        /// The same contracts as the per-file payloads the writer takes.
        payloads: Vec<(String, usize, Vec<u8>)>,
        /// `(path, slot)` of every file in the package, in slot order.
        files: Vec<(String, usize)>,
    }

    fn build_packages(parsed: &[(&'static str, SourceTree)]) -> Vec<BuiltPackage> {
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (slot, (path, _)) in parsed.iter().enumerate() {
            groups.entry(fallback_package_key(path)).or_default().push(slot);
        }
        groups
            .into_iter()
            .map(|(key, slots)| {
                let mut shard = PackageShard::default();
                let mut contracts = Vec::new();
                let mut payloads = Vec::new();
                let mut files = Vec::new();
                for slot in slots {
                    let (path, tree) = &parsed[slot];
                    shard.add_file(slot, path, tree);
                    contracts.extend(decl_contracts(slot, tree));
                    payloads.push(((*path).to_owned(), slot, contract_payload(tree)));
                    files.push(((*path).to_owned(), slot));
                }
                BuiltPackage { key, shard, contracts, payloads, files }
            })
            .collect()
    }

    fn write_package(
        dir: &TempDir,
        parsed: &[(&'static str, SourceTree)],
        p: &BuiltPackage,
    ) -> PathBuf {
        let trace: Vec<(String, usize, Vec<u8>)> = p
            .files
            .iter()
            .map(|(path, slot)| (path.clone(), *slot, trace_payload(&parsed[*slot].1)))
            .collect();
        let path = dir.path(&p.key.replace('/', "-"));
        build_sections(&p.shard, &p.payloads, &trace).write_to(&path).unwrap();
        path
    }

    fn open(path: &std::path::Path) -> ArtifactReader {
        ArtifactReader::open(path, DecodeBudget::default()).unwrap()
    }

    #[test]
    fn the_section_names_are_spellable() {
        assert_eq!(symbols_section().as_str(), SYMBOLS_SECTION);
        assert_eq!(contracts_section().as_str(), CONTRACTS_SECTION);
        assert_eq!(trace_section().as_str(), TRACE_SECTION);
    }

    /// Round-trip identity (issue #487 acceptance (a)): every section
    /// deserializes to a value equal to what was serialized — the shard by
    /// its derived `Eq`, the contracts by theirs, every file's tree by
    /// `SourceTree`'s. Floats compare by IEEE equality, which is exact here:
    /// the wire carries bit patterns, and no PHP literal spells NaN (the one
    /// value IEEE equality cannot pin).
    #[test]
    fn a_package_round_trips_through_its_artifact() {
        let tmp = TempDir::new("round-trip");
        let parsed = parsed_fixture();
        let packages = build_packages(&parsed);
        assert!(packages.len() > 1, "the fixture must split into packages");

        // The fixture is only honest if the corners are actually in it.
        let app = &parsed[0].1;
        let rendered = format!("{app:?}");
        assert!(rendered.contains("inf"), "the non-finite float literal survived lowering");
        assert!(app.functions()[0].docblock.is_some(), "a docblocked function");
        // The payload codec carries an enum by variant index, so every
        // `SiteKind` variant has to survive the disk boundary *by position*. It
        // only can if the fixture carries it.
        let sites = fixture_sites(&parsed);
        for kind in SiteKind::ALL {
            let carried = sites.iter().any(|s| s.kind.tag() == kind);
            assert!(carried, "the fixture must carry a {} site", kind.name());
        }
        assert!(
            sites.iter().any(|s| !s.guards.is_empty()),
            "the fixture must carry a site under a catch guard"
        );
        // The same holds for the trace IR's variants and fields: the equality
        // below tests only the ones the fixture lowers to.
        let kinds = fixture_stmt_kinds(&parsed);
        for (construct, lowered) in TRACE_CONSTRUCTS {
            assert!(kinds.iter().any(|k| lowered(k)), "the fixture must carry {construct}");
        }
        // The `new` edge `Widget::n`'s `new self()` lowers to rides the codec by
        // variant index too (issue #849).
        let construct_edge = sites.iter().any(|s| matches!(s.kind, SiteKind::New { .. }));
        assert!(construct_edge, "the fixture must carry a constructor edge");
        // Schema 20 (issue #603): `ret_top` sits between `ret` and `ret_span` on
        // both declarations and `RetHintKind::Top` precedes `Other`, so the
        // enforced tops have to be IN the fixture for the positions to be tested.
        let tops: usize = parsed
            .iter()
            .flat_map(|(_, t)| t.classes().iter().flat_map(|c| c.methods.iter()))
            .filter(|m| m.ret_top.is_some())
            .count();
        assert!(tops >= 2, "the fixture must carry `: array` and `: object` methods, has {tops}");

        for p in &packages {
            let path = write_package(&tmp, &parsed, p);
            let mut reader = open(&path);
            assert_eq!(read_shard(&mut reader).unwrap(), p.shard, "{}", p.key);
            assert_eq!(read_contracts(&mut reader).unwrap(), p.contracts, "{}", p.key);
            let index = TraceIndex::open(&mut reader).unwrap();
            let listed: Vec<(String, usize)> =
                index.files().map(|(f, s)| (f.to_owned(), s)).collect();
            assert_eq!(listed, p.files, "{}", p.key);
            for (file, slot) in &p.files {
                let tree = index.read_tree(&mut reader, file).unwrap();
                assert_eq!(tree, parsed[*slot].1, "{file}");
            }
        }

        // The contracts leg carries both owners and real inputs.
        let all: Vec<&DeclContract> = packages.iter().flat_map(|p| &p.contracts).collect();
        assert!(all.iter().any(|c| matches!(c.owner, ContractOwner::Function { .. })));
        assert!(all.iter().any(|c| matches!(c.owner, ContractOwner::Method { .. })));
        assert!(all.iter().any(|c| c.docblock.is_some() && !c.params.is_empty()));
        assert!(all.iter().any(|c| !c.ctx.class_imports.is_empty() || !c.ctx.namespace.is_empty()));
    }

    /// [`SiteKind::ALL`] is what the tests here enumerate for sites, in the
    /// order the codec numbers the variants by. A variant missing from it does
    /// not compile (`site_kinds!` in steins-syntax), but nothing there fixes the
    /// order, so this pins names and order against the variant list serde
    /// decodes a [`SiteKind`] by.
    #[test]
    fn the_site_kinds_are_every_variant_in_codec_order() {
        let names: Vec<&str> = SiteKind::ALL.iter().map(|k| k.name()).collect();
        assert_eq!(names, serde_variants::<SiteKind>());
    }

    /// Every site in the fixture round-trips through the payload codec on its
    /// own, kind by kind, so a kind that stopped decoding fails under its own
    /// name rather than somewhere inside a whole tree.
    #[test]
    fn every_site_kind_round_trips_through_the_codec() {
        let parsed = parsed_fixture();
        let sites = fixture_sites(&parsed);
        for kind in SiteKind::ALL {
            let name = kind.name();
            let of_kind: Vec<&SiteOrigin> =
                sites.iter().copied().filter(|s| s.kind.tag() == kind).collect();
            assert!(!of_kind.is_empty(), "the fixture must carry a {name} site");
            for site in of_kind {
                let bytes = crate::wire::to_vec(site).expect("a site serializes");
                let back: SiteOrigin = crate::wire::from_slice(&bytes).expect("a site round-trips");
                assert_eq!(&back, site, "{name}");
            }
        }
    }

    /// Operator sites carry two more enums, [`OperatorFamily`] and
    /// [`OperatorConstruct`], that the codec numbers by position; this pins their
    /// `ALL` lists to the variant order serde decodes them by.
    #[test]
    fn the_operator_enums_are_every_variant_in_codec_order() {
        let families: Vec<String> = OperatorFamily::ALL.iter().map(|v| format!("{v:?}")).collect();
        assert_eq!(families, serde_variants::<OperatorFamily>());
        let constructs: Vec<String> =
            OperatorConstruct::ALL.iter().map(|v| format!("{v:?}")).collect();
        assert_eq!(constructs, serde_variants::<OperatorConstruct>());
    }

    /// An operand's [`ArgShape`] is numbered by position too: the call-result
    /// variants (issue #877) are appended after `Unknown`, so no index a stored
    /// payload holds moves, and both survive the payload codec.
    #[test]
    fn the_arg_shape_call_variants_are_appended_and_round_trip() {
        use steins_syntax::{ArgShape, EffectRecv, NameRef, RefKind};
        let variants = serde_variants::<ArgShape>();
        let expected =
            ["ObjectFree", "Array", "Param", "Local", "ThisProperty", "Unknown", "Call", "MethodCall"];
        assert_eq!(variants, expected);
        let call = NameRef { raw: "strlen".to_owned(), kind: RefKind::Unqualified, offset: 7 };
        let shapes = [
            ArgShape::Call(call.clone()),
            ArgShape::MethodCall { receiver: EffectRecv::Var("e".to_owned()), method: "m".into() },
            ArgShape::MethodCall { receiver: EffectRecv::ClassName(call), method: "n".into() },
        ];
        for shape in shapes {
            let bytes = crate::wire::to_vec(&shape).expect("a shape serializes");
            let back: ArgShape = crate::wire::from_slice(&bytes).expect("a shape round-trips");
            assert_eq!(back, shape);
        }
    }

    /// A call's literal arguments are read by field position: the strict flags (`bools`) and
    /// the float evidence of a printf call (`float_evidence`, ADR-0101 §3.8) are appended after
    /// `ints`, and survive the payload codec.
    #[test]
    fn the_const_args_flags_and_float_evidence_round_trip() {
        use steins_syntax::{
            ArgShape, CallTarget, ConstArgs, ConstInt, FloatEvidence, StaticClass, Stored,
        };
        let param = ArgShape::Param { name: "s".to_owned(), stores: Stored::ObjectFree };
        let args = ConstArgs {
            first: Some(CallTarget::Literal("%s".to_owned())),
            second: None,
            ints: vec![(1, ConstInt::Int(8))],
            bools: vec![(2, true), (3, false)],
            float_evidence: vec![
                (1, FloatEvidence::NoFloat),
                (2, FloatEvidence::Float),
                (
                    3,
                    FloatEvidence::Shape {
                        shape: param,
                        unwritten: false,
                        writes: vec![FloatEvidence::GlobalConst(steins_syntax::NameRef {
                            raw: "PHP_EOL".to_owned(),
                            kind: steins_syntax::RefKind::Unqualified,
                            offset: 3,
                        })],
                    },
                ),
                (4, FloatEvidence::ClassConst { class: StaticClass::SelfKw, name: "K".to_owned() }),
                (
                    5,
                    FloatEvidence::StaticProperty {
                        class: StaticClass::Parent,
                        name: "p".to_owned(),
                    },
                ),
                (6, FloatEvidence::OneOf(vec![FloatEvidence::NoFloat, FloatEvidence::Float])),
            ],
        };
        let bytes = crate::wire::to_vec(&args).expect("const args serialize");
        let back: ConstArgs = crate::wire::from_slice(&bytes).expect("const args round-trip");
        assert_eq!(back, args);
        assert_eq!(
            serde_variants::<FloatEvidence>(),
            ["NoFloat", "Float", "Shape", "GlobalConst", "ClassConst", "StaticProperty", "OneOf"]
        );
    }

    /// Every family and every construct of an operator site is in the fixture and
    /// round-trips through the payload codec with its receivers and member name.
    #[test]
    fn every_operator_form_round_trips_through_the_codec() {
        let parsed = parsed_fixture();
        let sites = fixture_sites(&parsed);
        let operators: Vec<&SiteOrigin> =
            sites.iter().copied().filter(|s| matches!(s.kind, SiteKind::Operator { .. })).collect();
        for family in OperatorFamily::ALL {
            let carried = operators.iter().any(
                |s| matches!(&s.kind, SiteKind::Operator { family: f, .. } if *f == family),
            );
            assert!(carried, "the fixture must carry a {family:?} operator site");
        }
        for construct in OperatorConstruct::ALL {
            let carried = operators.iter().any(
                |s| matches!(&s.kind, SiteKind::Operator { construct: c, .. } if *c == construct),
            );
            assert!(carried, "the fixture must carry a {construct:?} operator site");
        }
        assert!(operators.iter().any(|s| {
            matches!(&s.kind, SiteKind::Operator { member: Some(_), receivers, .. }
                if receivers.iter().any(Option::is_some))
        }));
        for site in operators {
            let bytes = crate::wire::to_vec(site).expect("a site serializes");
            let back: SiteOrigin = crate::wire::from_slice(&bytes).expect("a site round-trips");
            assert_eq!(&back, site);
        }
    }

    /// The differential oracle across the disk boundary (issue #487
    /// acceptance (b), extending issue #486's): a `ProjectIndex`
    /// reconstructed from *deserialized* shards is identical to the cold
    /// build over the same units — and so is the whole merged-table set.
    #[test]
    fn the_warm_index_equals_the_cold_build_across_the_disk_boundary() {
        let tmp = TempDir::new("differential");
        let parsed = parsed_fixture();
        let packages = build_packages(&parsed);

        let decoded: Vec<PackageShard> = packages
            .iter()
            .map(|p| {
                let path = write_package(&tmp, &parsed, p);
                read_shard(&mut open(&path)).unwrap()
            })
            .collect();
        let original: Vec<PackageShard> = packages.iter().map(|p| p.shard.clone()).collect();
        assert_eq!(merge_shards(&decoded), merge_shards(&original));

        let db = SteinsDatabase::default();
        let files: Vec<SourceFile> = fixture()
            .iter()
            .map(|(p, t)| SourceFile::new(&db, (*p).to_owned(), (*t).to_owned()))
            .collect();
        let project = Project::new(
            &db,
            files.clone(),
            crate::ProjectLayout::fallback(),
            crate::PluginFacts::none(),
        );
        let cold = project_index(&db, project);
        let warm = ProjectIndex::from_merged(merge_shards(&decoded), &files);
        assert!(*cold == warm, "the disk boundary changed the index");
    }

    /// Acceptance (c) for the flat sections: arbitrary bytes, a payload of the
    /// previous codec, a truncated payload, one with bytes left over, and an
    /// absent section are each a `Miss` — never a panic, never a partial
    /// value. The truncation and trailing-byte cases are the ones a
    /// length-prefixed codec needs: without them a doctored length would be a
    /// *shorter* value rather than an error.
    #[test]
    fn a_doctored_flat_section_is_a_miss() {
        let tmp = TempDir::new("doctored-flat");
        let real_shard = crate::wire::to_vec(&PackageShard::default()).unwrap();
        let real_contracts = contract_payload(&SourceTree::parse("<?php function f() {}\n"));
        let mut short_shard = real_shard.clone();
        short_shard.truncate(real_shard.len() / 2);
        let mut short_contracts = real_contracts.clone();
        short_contracts.truncate(real_contracts.len() / 2);
        let mut long_contracts = real_contracts.clone();
        long_contracts.push(0);
        // The contracts cases go through the framing the reader expects, so
        // what they exercise is the *payload* codec and not the directory
        // check every unframed byte string would trip first.
        let framed = |payload: Vec<u8>| {
            payload_section_bytes(&[("a.php".to_owned(), 0usize, payload)])
        };
        let cases = vec![
            ("symbols-arbitrary-bytes", vec![(symbols_section(), b"not a shard".to_vec())]),
            // The previous codec's bytes: an artifact of an older schema never
            // reaches a payload reader, but the reader must not be the reason
            // for that.
            (
                "symbols-previous-codec",
                vec![(symbols_section(), serde_json::to_vec(&PackageShard::default()).unwrap())],
            ),
            ("symbols-truncated", vec![(symbols_section(), short_shard)]),
            ("contracts-arbitrary-bytes", vec![(contracts_section(), framed(b"}".to_vec()))]),
            ("contracts-truncated", vec![(contracts_section(), framed(short_contracts))]),
            ("contracts-trailing-bytes", vec![(contracts_section(), framed(long_contracts))]),
        ];
        for (tag, sections) in cases {
            let path = tmp.path(tag);
            let mut builder = ArtifactBuilder::new();
            for (name, bytes) in sections {
                builder.section(name, bytes).unwrap();
            }
            builder.write_to(&path).unwrap();
            let mut reader = open(&path);
            assert!(read_shard(&mut reader).is_err(), "{tag}: shard");
            assert!(read_contracts(&mut reader).is_err(), "{tag}: contracts");
            assert!(TraceIndex::open(&mut reader).is_err(), "{tag}: trace");
        }
    }

    /// Acceptance (c) for the nested trace directory: every way the framing
    /// can lie — a section shorter than its prefix, a prefix that overruns, a
    /// directory that is not one (or carries a field this schema does not),
    /// offsets that do not tile, a duplicate path, payload bytes left over or
    /// missing — is a `Miss` at open.
    #[test]
    fn a_doctored_trace_directory_is_a_miss() {
        let tmp = TempDir::new("doctored-trace");
        let entry = |path: &str, offset: u64, len: u64| {
            format!(r#"{{"path": "{path}", "slot": 0, "offset": {offset}, "len": {len}}}"#)
        };
        let framed = |dir: &str, payload: &[u8]| {
            let mut bytes = (dir.len() as u64).to_le_bytes().to_vec();
            bytes.extend_from_slice(dir.as_bytes());
            bytes.extend_from_slice(payload);
            bytes
        };
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("shorter-than-prefix", b"abc".to_vec()),
            ("prefix-overruns", 999u64.to_le_bytes().to_vec()),
            ("directory-not-json", framed("not json", b"")),
            ("directory-not-a-list", framed("{}", b"")),
            (
                "directory-extra-field",
                framed(
                    r#"[{"path": "a.php", "slot": 0, "offset": 0, "len": 2, "extra": 1}]"#,
                    b"{}",
                ),
            ),
            ("first-offset-nonzero", framed(&format!("[{}]", entry("a.php", 1, 1)), b"xx")),
            (
                "offsets-do-not-tile",
                framed(&format!("[{}, {}]", entry("a.php", 0, 1), entry("b.php", 2, 1)), b"xxx"),
            ),
            (
                "duplicate-path",
                framed(&format!("[{}, {}]", entry("a.php", 0, 1), entry("a.php", 1, 1)), b"xx"),
            ),
            ("payload-bytes-left-over", framed(&format!("[{}]", entry("a.php", 0, 1)), b"xx")),
            ("payload-bytes-missing", framed(&format!("[{}]", entry("a.php", 0, 3)), b"xx")),
        ];
        for (tag, trace_bytes) in cases {
            let path = tmp.path(tag);
            let mut builder = ArtifactBuilder::new();
            builder.section(trace_section(), trace_bytes).unwrap();
            builder.write_to(&path).unwrap();
            let mut reader = open(&path);
            assert!(TraceIndex::open(&mut reader).is_err(), "{tag}");
        }
    }

    /// A corrupt *payload* is a miss for that file alone: the directory still
    /// opens, the intact neighbours still decode — the per-file nesting doing
    /// exactly what it exists for.
    #[test]
    fn a_corrupt_file_payload_is_a_miss_for_that_file_alone() {
        let tmp = TempDir::new("one-bad-payload");
        let tree = SourceTree::parse("<?php function ok() {}\n");
        let good = trace_payload(&tree);
        let parts = vec![
            ("good.php".to_owned(), 0usize, good),
            ("bad.php".to_owned(), 1usize, b"not a tree".to_vec()),
            ("shape.php".to_owned(), 2usize, b"[1, 2]".to_vec()),
        ];
        let path = tmp.path("pkg");
        let mut builder = ArtifactBuilder::new();
        builder.section(trace_section(), payload_section_bytes(&parts)).unwrap();
        builder.write_to(&path).unwrap();

        let mut reader = open(&path);
        let index = TraceIndex::open(&mut reader).unwrap();
        assert_eq!(index.read_tree(&mut reader, "good.php").unwrap(), tree);
        assert!(index.read_tree(&mut reader, "bad.php").is_err());
        assert!(index.read_tree(&mut reader, "shape.php").is_err());
        assert!(index.read_tree(&mut reader, "absent.php").is_err());
        assert_eq!(index.read_tree(&mut reader, "good.php").unwrap(), tree, "still serves");
    }

    /// The read transaction: loads lazily (a metadata question decodes no
    /// tree), reads one file without paying for the rest, and misses honestly
    /// on an unlisted path. Exercised over a *published generation*, so the
    /// store seam is crossed too; ownership needs no assertion — the
    /// transaction owns its values and drop frees them, with nothing global
    /// to leak.
    #[test]
    fn the_read_txn_loads_lazily_over_a_published_generation() {
        let tmp = TempDir::new("read-txn");
        let parsed = parsed_fixture();
        let packages = build_packages(&parsed);

        let id = GenerationInputs {
            analyzer_version: "test".to_owned(),
            packages: vec![],
            composer_lock: None,
            catalog_pin: "pin".to_owned(),
            plugins: vec![],
            engine: EnginePosture::Off,
            config: vec![],
        }
        .generation_id();
        let store = Store::open(&tmp.dir).unwrap();
        let mut candidate = store.begin(id, vec![]).unwrap();
        for p in &packages {
            let trace: Vec<(String, usize, Vec<u8>)> = p
                .files
                .iter()
                .map(|(path, slot)| (path.clone(), *slot, trace_payload(&parsed[*slot].1)))
                .collect();
            let name = PackageName::new(&p.key).unwrap();
            candidate
                .write_artifact(&name, &build_sections(&p.shard, &p.payloads, &trace))
                .unwrap();
        }
        let generation = candidate.publish().unwrap();

        let vendor = packages.iter().find(|p| p.key == "lib/a").unwrap();
        let mut txn = ReadTxn::open(&generation, &PackageName::new("lib/a").unwrap()).unwrap();
        assert_eq!(txn.shard().unwrap(), &vendor.shard);
        assert_eq!(txn.contracts().unwrap(), &vendor.contracts[..]);
        assert_eq!(txn.resident_trees(), 0, "metadata questions decode no tree");
        let (path0, slot0) = vendor.files[0].clone();
        assert_eq!(txn.tree(&path0).unwrap(), &parsed[slot0].1);
        assert_eq!(txn.resident_trees(), 1);
        assert_eq!(txn.tree(&path0).unwrap(), &parsed[slot0].1, "second ask is the cached value");
        assert_eq!(txn.resident_trees(), 1);
        assert!(txn.tree("src/app.php").is_err(), "another package's file is not here");
        assert!(
            ReadTxn::open(&generation, &PackageName::new("no/such").unwrap()).is_err(),
            "an absent package is a miss"
        );
    }

    #[test]
    fn residency_policy_defaults_follow_the_package_kind() {
        assert_eq!(
            ResidencyPolicy::default_for(PackageKind::Root),
            ResidencyPolicy::FirstPartyResident
        );
        assert_eq!(
            ResidencyPolicy::default_for(PackageKind::PathRepository),
            ResidencyPolicy::FirstPartyResident
        );
        assert_eq!(
            ResidencyPolicy::default_for(PackageKind::Vendor),
            ResidencyPolicy::VendorOffloadable
        );
        assert_eq!(
            ResidencyPolicy::default_for(PackageKind::VendorStray),
            ResidencyPolicy::VendorOffloadable
        );
        assert_eq!(ResidencyPolicy::FirstPartyResident.as_str(), "first-party-resident");
        assert_eq!(ResidencyPolicy::VendorOffloadable.as_str(), "vendor-offloadable");
    }
}
