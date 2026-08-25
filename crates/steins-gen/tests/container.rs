//! The artifact container: round-trip, seeking, and the promise that every
//! way a file can be wrong comes back as a `Miss`, never a panic.

use std::path::PathBuf;

use steins_gen::{ArtifactBuilder, ArtifactReader, DecodeBudget, Miss, SectionName};

/// A throwaway directory under the OS temp dir, cleaned on drop.
struct TempDir {
    dir: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "steins-gen-container-{tag}-{}-{}",
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

fn sec(name: &str) -> SectionName {
    SectionName::new(name).unwrap()
}

/// Three sections written, then a fresh artifact file on disk.
fn sample_artifact(dir: &TempDir, file: &str) -> PathBuf {
    let mut builder = ArtifactBuilder::new();
    builder.section(sec("symbols"), b"the symbol bytes".to_vec()).unwrap();
    builder.section(sec("summaries"), vec![0xAB; 4096]).unwrap();
    builder.section(sec("empty"), Vec::new()).unwrap();
    let path = dir.path(file);
    builder.write_to(&path).unwrap();
    path
}

#[test]
fn round_trip_reads_every_section_by_name() {
    let dir = TempDir::new("round-trip");
    let path = sample_artifact(&dir, "a.pkg");
    let mut reader = ArtifactReader::open(&path, DecodeBudget::default()).unwrap();
    let names: Vec<String> = reader.sections().map(|n| n.as_str().to_owned()).collect();
    assert_eq!(names, ["symbols", "summaries", "empty"]);
    assert_eq!(reader.section(&sec("symbols")).unwrap(), b"the symbol bytes");
    assert_eq!(reader.section(&sec("summaries")).unwrap(), vec![0xAB; 4096]);
    assert_eq!(reader.section(&sec("empty")).unwrap(), Vec::<u8>::new());
    assert_eq!(reader.section_len(&sec("summaries")), Some(4096));
    assert!(reader.has_section(&sec("symbols")));
    assert!(!reader.has_section(&sec("absent")));
}

/// Sections read out of write order too — the directory is the access path,
/// not the byte order.
#[test]
fn sections_read_in_any_order() {
    let dir = TempDir::new("any-order");
    let path = sample_artifact(&dir, "a.pkg");
    let mut reader = ArtifactReader::open(&path, DecodeBudget::default()).unwrap();
    assert_eq!(reader.section(&sec("empty")).unwrap(), Vec::<u8>::new());
    assert_eq!(reader.section(&sec("symbols")).unwrap(), b"the symbol bytes");
    assert_eq!(reader.section(&sec("symbols")).unwrap(), b"the symbol bytes");
}

/// Seeking means seeking: scribbling over one section's payload does not
/// disturb reads of the others, because nothing decodes what it wasn't asked
/// for.
#[test]
fn one_corrupt_section_leaves_the_others_readable() {
    let dir = TempDir::new("scribble");
    let path = sample_artifact(&dir, "a.pkg");
    let mut bytes = std::fs::read(&path).unwrap();
    let payload_start = 16 + 3 * 32; // header + directory
    for b in &mut bytes[payload_start + 16..payload_start + 16 + 4096] {
        *b ^= 0xFF; // ruin "summaries", leave "symbols" and "empty" alone
    }
    std::fs::write(&path, &bytes).unwrap();
    let mut reader = ArtifactReader::open(&path, DecodeBudget::default()).unwrap();
    assert_eq!(reader.section(&sec("symbols")).unwrap(), b"the symbol bytes");
    assert_eq!(reader.section(&sec("empty")).unwrap(), Vec::<u8>::new());
    assert_eq!(reader.section(&sec("summaries")).unwrap(), vec![0x54; 4096]);
}

#[test]
fn empty_container_round_trips() {
    let dir = TempDir::new("empty");
    let path = dir.path("a.pkg");
    ArtifactBuilder::new().write_to(&path).unwrap();
    let reader = ArtifactReader::open(&path, DecodeBudget::default()).unwrap();
    assert_eq!(reader.sections().count(), 0);
}

#[test]
fn schema_mismatch_is_a_miss() {
    let dir = TempDir::new("schema");
    let path = sample_artifact(&dir, "a.pkg");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[8..12].copy_from_slice(&2u32.to_le_bytes());
    std::fs::write(&path, &bytes).unwrap();
    match ArtifactReader::open(&path, DecodeBudget::default()) {
        Err(Miss::SchemaMismatch { found: 2 }) => {}
        other => panic!("expected SchemaMismatch, got {:?}", other.map(|_| ())),
    }
}

#[test]
fn foreign_magic_is_a_miss() {
    let dir = TempDir::new("magic");
    let path = sample_artifact(&dir, "a.pkg");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[0] = b'X';
    std::fs::write(&path, &bytes).unwrap();
    assert!(matches!(ArtifactReader::open(&path, DecodeBudget::default()), Err(Miss::BadMagic)));
}

/// Every truncation point — mid-magic, mid-header, mid-directory,
/// mid-payload — is a miss, and none of them panics.
#[test]
fn truncation_at_every_length_is_a_miss_never_a_panic() {
    let dir = TempDir::new("truncate");
    let path = sample_artifact(&dir, "a.pkg");
    let full = std::fs::read(&path).unwrap();
    for len in 0..full.len() {
        let cut = dir.path("cut.pkg");
        std::fs::write(&cut, &full[..len]).unwrap();
        assert!(
            ArtifactReader::open(&cut, DecodeBudget::default()).is_err(),
            "a file truncated to {len} of {} bytes must miss",
            full.len()
        );
    }
}

#[test]
fn trailing_garbage_is_a_miss() {
    let dir = TempDir::new("trailing");
    let path = sample_artifact(&dir, "a.pkg");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.push(0);
    std::fs::write(&path, &bytes).unwrap();
    assert!(ArtifactReader::open(&path, DecodeBudget::default()).is_err());
}

#[test]
fn garbage_file_is_a_miss() {
    let dir = TempDir::new("garbage");
    let path = dir.path("junk.pkg");
    let junk: Vec<u8> = (0u32..4096).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    std::fs::write(&path, &junk).unwrap();
    assert!(ArtifactReader::open(&path, DecodeBudget::default()).is_err());
}

#[test]
fn absent_file_is_a_miss() {
    let dir = TempDir::new("absent-file");
    assert!(ArtifactReader::open(&dir.path("never.pkg"), DecodeBudget::default()).is_err());
}

/// A directory whose offsets lie — a hole between sections — is corrupt,
/// even though every range is in bounds.
#[test]
fn lying_directory_offsets_are_a_miss() {
    let dir = TempDir::new("lying-dir");
    let path = dir.path("a.pkg");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"steinsgn");
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    let mut name = [0u8; 16];
    name[..3].copy_from_slice(b"sym");
    bytes.extend_from_slice(&name);
    bytes.extend_from_slice(&56u64.to_le_bytes()); // real payload starts at 48
    bytes.extend_from_slice(&8u64.to_le_bytes());
    bytes.extend_from_slice(&[0u8; 16]); // 8 of hole + 8 of "payload"
    assert!(ArtifactReader::open(&path, DecodeBudget::default()).is_err()); // absent yet
    std::fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        ArtifactReader::open(&path, DecodeBudget::default()),
        Err(Miss::Corrupt(_))
    ));
}

/// The allocation ceiling is checked before anything is read: a file over
/// budget is refused at open.
#[test]
fn over_budget_file_is_a_miss() {
    let dir = TempDir::new("budget");
    let path = sample_artifact(&dir, "a.pkg");
    let tiny = DecodeBudget { max_file_bytes: 64 };
    assert!(matches!(
        ArtifactReader::open(&path, tiny),
        Err(Miss::OverBudget { ceiling: 64, .. })
    ));
}

#[test]
fn absent_section_is_a_miss() {
    let dir = TempDir::new("absent-section");
    let path = sample_artifact(&dir, "a.pkg");
    let mut reader = ArtifactReader::open(&path, DecodeBudget::default()).unwrap();
    assert!(matches!(reader.section(&sec("folds")), Err(Miss::AbsentSection(_))));
}

#[test]
fn duplicate_section_is_rejected_at_build() {
    let mut builder = ArtifactBuilder::new();
    builder.section(sec("symbols"), b"a".to_vec()).unwrap();
    assert!(builder.section(sec("symbols"), b"b".to_vec()).is_err());
}
