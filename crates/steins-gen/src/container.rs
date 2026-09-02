//! The payload-agnostic artifact container: one file per package, a section a
//! named byte range. Layout, all integers little-endian:
//!
//! ```text
//! magic      8 bytes   b"steinsgn"
//! schema     u32       exact match against SCHEMA_VERSION, or miss
//! count      u32       number of directory entries
//! directory  count × { name 16 bytes (NUL-padded), offset u64, len u64 }
//! sections   contiguous payload bytes, in directory order
//! ```
//!
//! A reader seeks to one section without decoding any other; what the bytes
//! *mean* is the payload owner's business and explicitly a cache format — no
//! migration, no cross-version reads. Decoding is bounded: the per-file
//! ceiling ([`DecodeBudget`]) is checked before any allocation, and every
//! violation or parse failure is a [`Miss`], never a panic.

use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::names::{PackageName, SectionName};

/// The artifact schema version. Covers everything this crate writes — the
/// container layout, the manifest, `CURRENT` — and participates in the
/// generation fingerprint, so bumping it obsoletes every stored generation at
/// once. A mismatch is a miss; there is no migration path by design
/// (ADR-0092 §2).
///
/// It also covers what the payload owners write *inside* a section, because a
/// stored generation is only useful if every reader agrees with every writer:
/// `2` was the swap of the payload codec from serde_json to `steins_db::wire`
/// (issue #504), and `3` is the split of the run-dependent walk blocks out of
/// the artifact into a sidecar container (issue #519), which is what leaves an
/// unmoved package's artifact byte-identical between generations and therefore
/// shareable. `4` is the `CondExpr::IssetVar` variant (issue #414): the stored
/// trace IR spells a bare `isset($x)` differently now, and a reader that
/// disagrees with its writer about what that condition IS would answer from a
/// forgetting an artifact recorded and this binary no longer performs, and `5`
/// is `CondExpr::InstanceofDyn` (issue #571) for the same reason one spelling
/// over. `6` is the offset-argument entry in `Stmt::invalidated` (issue #609):
/// a stored trace of `sort($a[0])` from schema 5 carries no entry for `$a`, so
/// replaying it would keep the stale array shape this binary now forgets. `7` is
/// the `ArgValue::Isset` value carrier (issue #579), the value-position twin of
/// `4`'s reason: a schema-6 trace spells `$b = isset($a['k'])` as
/// `ArgValue::Other`, so replaying it would answer `unknown` for a value this
/// binary decides. `8` is `ValueOp::BitOr` (issue #615), the same reason once
/// more and with a wider blast radius than the operator itself: a `|` used to
/// lower to `ArgValue::Other`, and an `Other` ELEMENT collapses its whole
/// enclosing array literal to `Other`, so a schema-7 trace spells
/// `['flags' => FILTER_A | FILTER_B]` as no array at all. `9` is the literal-spread
/// flattening (issue #616): `f(1, ...[2, 3])` used to lower to `ArgValue::Other`
/// with `has_spread` raised and now lowers to the three-argument call it names, so
/// a schema-8 trace both spells the value differently and reports an argument count
/// this binary no longer believes unproven. `10` is the logical family (issue
/// #625) — `ArgValue::Logical`, `ArgValue::Not` and `ValueOp::Spaceship`, bumped
/// ONCE for all three because they land together — and it is the same reason a
/// fourth time: a schema-9 trace spells `$a && $b`, `!$x` and `$a <=> $b` as
/// `ArgValue::Other`, so replaying it would answer `unknown` for three values
/// this binary now decides, and would miss the dead right operand of a decided
/// `&&`/`||` that only the new carrier's span records.
/// `11` is the auto-index append (issue #636): `StmtKind::OffsetAppend`. A schema-10
/// trace spells `$a[] = 1` as `StmtKind::Barrier` — a statement that clears the whole
/// environment — so replaying one would answer `unknown` for every local from that
/// line on, where this binary answers the extended array. The variant is new, not
/// re-encoded, so an old trace cannot be misread as the new form; it simply states
/// something weaker than the source does.
/// `12` is the value-position cast carrier (issue #626): `ArgValue::Cast`. A schema-11
/// trace spells `(int) $x` as `ArgValue::Other` — every cast expression did — so
/// replaying one would answer `unknown` where this binary answers the conversion grid,
/// or at worst the base type the cast operator guarantees.
/// Bumping it is the whole migration — an artifact of the previous schema becomes an
/// ordinary [`Miss`] and one rebuild.
pub const SCHEMA_VERSION: u32 = 12;

const MAGIC: [u8; 8] = *b"steinsgn";
const HEADER_LEN: u64 = 16;
const DIR_ENTRY_LEN: u64 = 32;
const NAME_FIELD_LEN: usize = 16;

/// Why a read degraded to rebuild-from-source. Every variant means the same
/// thing to the caller — rebuild; the standing invariant is that a miss may
/// change cost, never meaning (ADR-0092 §2). The reasons exist so `doctor`
/// can one day say *why* the cache did not serve.
#[derive(Debug)]
pub enum Miss {
    Io(io::Error),
    BadMagic,
    SchemaMismatch { found: u32 },
    Truncated,
    Corrupt(&'static str),
    OverBudget { need: u64, ceiling: u64 },
    AbsentSection(SectionName),
    AbsentPackage(PackageName),
    AbsentGeneration,
}

impl fmt::Display for Miss {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Miss::Io(e) => write!(f, "io: {e}"),
            Miss::BadMagic => f.write_str("not a steins-gen artifact (bad magic)"),
            Miss::SchemaMismatch { found } => {
                write!(f, "schema {found}, this build reads only {SCHEMA_VERSION}")
            }
            Miss::Truncated => f.write_str("truncated"),
            Miss::Corrupt(what) => write!(f, "corrupt: {what}"),
            Miss::OverBudget { need, ceiling } => {
                write!(f, "{need} bytes exceeds the decode ceiling of {ceiling}")
            }
            Miss::AbsentSection(name) => write!(f, "no section named {name}"),
            Miss::AbsentPackage(name) => write!(f, "no package named {name}"),
            Miss::AbsentGeneration => f.write_str("the named generation is not in the store"),
        }
    }
}

impl std::error::Error for Miss {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Miss::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Miss {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::UnexpectedEof => Miss::Truncated,
            _ => Miss::Io(e),
        }
    }
}

/// The per-file allocation ceiling, checked against a file's stat length
/// before anything is read and hence before anything is allocated. A file
/// over the ceiling is a miss, not an attempt.
#[derive(Debug, Clone, Copy)]
pub struct DecodeBudget {
    pub max_file_bytes: u64,
}

impl Default for DecodeBudget {
    /// 1 GiB — far above any artifact the builder writes today, low enough
    /// that a corrupt length field cannot ask for the address space.
    fn default() -> Self { Self { max_file_bytes: 1 << 30 } }
}

/// A section was added twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateSection(pub SectionName);

impl fmt::Display for DuplicateSection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "section {} added twice", self.0)
    }
}

impl std::error::Error for DuplicateSection {}

/// Accumulates sections in memory and writes the complete container in one
/// pass — the file exists on disk only in its finished form, fsynced, so a
/// torn write can never masquerade as a short artifact. (Torn *candidates*
/// are the store's problem; see the in-progress marker.)
///
/// Not every artifact in a generation goes through here: one that another
/// generation already holds byte for byte is shared rather than rebuilt
/// (`Candidate::adopt_artifact`), which writes no bytes and needs no barrier.
#[derive(Default)]
pub struct ArtifactBuilder {
    sections: Vec<(SectionName, Vec<u8>)>,
}

impl ArtifactBuilder {
    pub fn new() -> Self { Self::default() }

    /// Add one section. Order is preserved on disk; names must be unique.
    pub fn section(&mut self, name: SectionName, bytes: Vec<u8>) -> Result<(), DuplicateSection> {
        if self.sections.iter().any(|(n, _)| *n == name) {
            return Err(DuplicateSection(name));
        }
        self.sections.push((name, bytes));
        Ok(())
    }

    /// Write the container to `path` and fsync it. The store calls this for
    /// you via `Candidate::write_artifact`; it is public so tests and tools
    /// can build containers anywhere.
    ///
    /// `path` must not exist: the open is `O_CREAT | O_EXCL`, so a write can
    /// never truncate a name that is already there. That is what makes an
    /// artifact *shared* with another generation (`crate::share`) safe to
    /// alias — the only writer in this crate cannot reach an existing inode,
    /// hard link or not — and it turns a double write into a loud
    /// `AlreadyExists` rather than a silent overwrite.
    ///
    /// The fsync is the one durability barrier the store keeps, and it is here
    /// rather than in the publish chain for a reason: section bytes are the
    /// only thing in a generation whose loss could be *undetectable*. Every
    /// other file the store writes is strictly parsed and self-identifying, so
    /// a torn one is a [`Miss`] and a rebuild; a torn payload could in
    /// principle decode. See `crate::store` for the full argument.
    pub fn write_to(&self, path: &Path) -> io::Result<()> {
        let mut file = File::create_new(path)?;
        let mut out = io::BufWriter::new(&mut file);
        out.write_all(&MAGIC)?;
        out.write_all(&SCHEMA_VERSION.to_le_bytes())?;
        out.write_all(&(self.sections.len() as u32).to_le_bytes())?;
        let mut offset = HEADER_LEN + DIR_ENTRY_LEN * self.sections.len() as u64;
        for (name, bytes) in &self.sections {
            let mut field = [0u8; NAME_FIELD_LEN];
            field[..name.as_str().len()].copy_from_slice(name.as_str().as_bytes());
            out.write_all(&field)?;
            out.write_all(&offset.to_le_bytes())?;
            out.write_all(&(bytes.len() as u64).to_le_bytes())?;
            offset += bytes.len() as u64;
        }
        for (_, bytes) in &self.sections {
            out.write_all(bytes)?;
        }
        out.flush()?;
        drop(out);
        file.sync_all()
    }
}

struct DirEntry {
    name: SectionName,
    offset: u64,
    len: u64,
}

/// Reads one artifact: validates the header and directory up front (strictly —
/// sections must tile the file exactly as the writer laid them), then serves
/// individual sections by seek + exact read.
pub struct ArtifactReader {
    file: File,
    directory: Vec<DirEntry>,
}

impl ArtifactReader {
    /// Open and validate. Everything that can be wrong — absent file, foreign
    /// magic, schema drift, truncation, a directory that lies about its
    /// offsets, a file over `budget` — comes back as a [`Miss`].
    pub fn open(path: &Path, budget: DecodeBudget) -> Result<Self, Miss> {
        let mut file = File::open(path)?;
        let file_len = file.metadata()?.len();
        if file_len > budget.max_file_bytes {
            return Err(Miss::OverBudget { need: file_len, ceiling: budget.max_file_bytes });
        }
        let mut header = [0u8; HEADER_LEN as usize];
        file.read_exact(&mut header)?;
        if header[0..8] != MAGIC {
            return Err(Miss::BadMagic);
        }
        let schema = u32::from_le_bytes(header[8..12].try_into().unwrap());
        if schema != SCHEMA_VERSION {
            return Err(Miss::SchemaMismatch { found: schema });
        }
        let count = u32::from_le_bytes(header[12..16].try_into().unwrap()) as u64;
        // The directory allocation is bounded by this check: count entries
        // must fit in what the stat length promised.
        if HEADER_LEN + count * DIR_ENTRY_LEN > file_len {
            return Err(Miss::Truncated);
        }
        let mut directory = Vec::with_capacity(count as usize);
        let mut expected_offset = HEADER_LEN + count * DIR_ENTRY_LEN;
        let mut raw = [0u8; DIR_ENTRY_LEN as usize];
        for _ in 0..count {
            file.read_exact(&mut raw)?;
            let name = parse_name_field(&raw[..NAME_FIELD_LEN])?;
            let offset = u64::from_le_bytes(raw[16..24].try_into().unwrap());
            let len = u64::from_le_bytes(raw[24..32].try_into().unwrap());
            if offset != expected_offset {
                return Err(Miss::Corrupt("directory offsets do not tile the file"));
            }
            expected_offset =
                offset.checked_add(len).ok_or(Miss::Corrupt("section length overflows"))?;
            if directory.iter().any(|e: &DirEntry| e.name == name) {
                return Err(Miss::Corrupt("duplicate section name"));
            }
            directory.push(DirEntry { name, offset, len });
        }
        if expected_offset != file_len {
            return Err(Miss::Truncated);
        }
        Ok(Self { file, directory })
    }

    /// The section names, in on-disk order.
    pub fn sections(&self) -> impl Iterator<Item = &SectionName> {
        self.directory.iter().map(|e| &e.name)
    }

    pub fn has_section(&self, name: &SectionName) -> bool {
        self.directory.iter().any(|e| e.name == *name)
    }

    /// A section's byte length, without reading it.
    pub fn section_len(&self, name: &SectionName) -> Option<u64> {
        self.directory.iter().find(|e| e.name == *name).map(|e| e.len)
    }

    /// Read one section's bytes: one seek, one exact read, nothing else
    /// touched. The bytes are the payload owner's to decode — and the owner's
    /// parse failures are misses too, by the same invariant.
    pub fn section(&mut self, name: &SectionName) -> Result<Vec<u8>, Miss> {
        let entry = self
            .directory
            .iter()
            .find(|e| e.name == *name)
            .ok_or_else(|| Miss::AbsentSection(name.clone()))?;
        let (offset, len) = (entry.offset, entry.len);
        self.read_range(offset, len)
    }

    /// Read `len` bytes at `offset` *within* the named section (offset 0 is
    /// the section's first byte). The sub-range primitive a payload's own
    /// nested index stands on (issue #487): the trace payload keeps a
    /// per-file directory inside one section, and a reader after one file
    /// must not pay for the rest. Still payload-agnostic — what this crate
    /// serves is a bounded window into a named range, nothing about what the
    /// window means. A range outside the section's extent is a [`Miss`].
    pub fn section_slice(
        &mut self,
        name: &SectionName,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>, Miss> {
        let entry = self
            .directory
            .iter()
            .find(|e| e.name == *name)
            .ok_or_else(|| Miss::AbsentSection(name.clone()))?;
        let end = offset.checked_add(len).ok_or(Miss::Corrupt("sub-range overflows"))?;
        if end > entry.len {
            return Err(Miss::Corrupt("sub-range outside its section"));
        }
        let start = entry.offset + offset;
        self.read_range(start, len)
    }

    /// One seek, one exact read of `len` bytes at the absolute `offset`.
    /// Callers have already bounds-checked the range against the directory,
    /// which `open` validated against the file's stat length (and hence the
    /// decode budget), so the allocation here is budget-bounded.
    fn read_range(&mut self, offset: u64, len: u64) -> Result<Vec<u8>, Miss> {
        let len = usize::try_from(len).map_err(|_| Miss::Corrupt("section length overflows"))?;
        let mut bytes = vec![0u8; len];
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(&mut bytes)?;
        Ok(bytes)
    }
}

fn parse_name_field(field: &[u8]) -> Result<SectionName, Miss> {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    if field[end..].iter().any(|&b| b != 0) {
        return Err(Miss::Corrupt("garbage after NUL in a section name"));
    }
    let name = str::from_utf8(&field[..end]).map_err(|_| Miss::Corrupt("section name not UTF-8"))?;
    SectionName::new(name).map_err(|_| Miss::Corrupt("section name outside [a-z0-9_-]{1,16}"))
}
