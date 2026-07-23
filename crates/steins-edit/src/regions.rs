//! Region model (ADR-0047 Slice A): the pure config→region assignment.
//!
//! A partitioned run divides the file universe into regions ([`RegionId`]) whose
//! *nameability* the scoping rule (ADR-0047 §2) will consult. This module owns
//! only the assignment — a pure function of config + file path (ADR-0047 §6):
//! given the declared partition path-sets and observer path-sets, [`PartitionMap`]
//! answers "which region does this file's declaring scope belong to?".
//!
//! ## Region kinds (ADR-0047 §1)
//! - **Partition** Pᵢ — a user-declared, disjoint entry-point root.
//! - **Shared** S — every first-party file no partition claims; the safe default
//!   direction (unclaimed code keeps whole-universe preconditions). *Vendor* lives
//!   inside S carrying its own presumption flag (ADR-0047 §5), so it is one
//!   [`RegionId::Shared`] value with `vendor: true`.
//! - **Observer** O — declared tests / dev-scripts that may reference any
//!   partition.
//!
//! ## Assignment precedence (this module's definition; not yet consumed)
//! Slice A wires the map through to the planners but no planner *decides* on it
//! (ADR-0047 §6 — "with one region the planner degenerates to today's behavior").
//! The order is nonetheless fixed and deterministic:
//! 1. **Vendor always wins.** A `vendor/` file is `Shared { vendor: true }` even
//!    when a partition glob accidentally covers it — vendor is *always* shared
//!    (ADR-0047 §1/§5), so a partition never claims vendor code.
//! 2. **Observer** — a file matching a declared observer glob is an observer
//!    (a test inside a service tree is a test, not that service's private code).
//! 3. **Partition** — a first-party file matching exactly one partition's globs.
//! 4. **Shared** — every remaining first-party file (`vendor: false`).
//!
//! ## Glob syntax (the minimal subset ADR-0047 §7's example needs)
//! Partition and observer sets are **directory-prefix globs**, matching the
//! dialect the repo already uses for `exclude`/`[paths.sets]`:
//! - `*`  — any run of characters **except** the path separator `/`.
//! - `**` — any run **including** `/` (spans directories); the ADR example's
//!   `svc-a.example/**`, `batch/**`, `tests/**` form.
//!
//! No `?`, character classes, or brace expansion. Patterns and paths use `/`.
//!
//! Overlap detection (ADR-0047 §7 — "overlapping partition globs are a config
//! error") is computed on each glob's **literal segment prefix** (the leading
//! `/`-segments before the first wildcard): two partition globs conflict when one
//! prefix is a segment-prefix of the other. This is exact for the documented
//! `dir/**` form; a pattern beginning with a wildcard has an empty prefix and so
//! is treated as overlapping every partition (deliberately conservative).

use steins_infer::is_vendor_path;

/// Which region a file's declaring scope belongs to (ADR-0047 §1). *Vendor* is
/// not a separate variant: it is [`RegionId::Shared`] with `vendor: true`, since
/// vendor lives inside Shared but carries its own presumption (ADR-0047 §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionId {
    /// A declared partition (entry-point root), identified by its config name.
    Partition(String),
    /// Shared code. `vendor: true` marks the `vendor/` tree (ADR-0047 §5
    /// presumption); `vendor: false` is first-party code no partition claims.
    Shared { vendor: bool },
    /// A declared observer (tests, dev-scripts; ADR-0047 §1/§4).
    Observer,
}

impl RegionId {
    /// First-party unclaimed shared code (`Shared { vendor: false }`).
    #[must_use]
    pub const fn shared() -> Self {
        Self::Shared { vendor: false }
    }

    /// The vendor tree (`Shared { vendor: true }`).
    #[must_use]
    pub const fn vendor() -> Self {
        Self::Shared { vendor: true }
    }

    /// Whether this region is inside Shared (first-party shared *or* vendor).
    #[must_use]
    pub const fn is_shared(&self) -> bool {
        matches!(self, Self::Shared { .. })
    }

    /// Whether this is the vendor tree specifically.
    #[must_use]
    pub const fn is_vendor(&self) -> bool {
        matches!(self, Self::Shared { vendor: true })
    }

    /// Whether this is an observer region.
    #[must_use]
    pub const fn is_observer(&self) -> bool {
        matches!(self, Self::Observer)
    }

    /// The partition name if this is a partition region.
    #[must_use]
    pub fn partition_name(&self) -> Option<&str> {
        match self {
            Self::Partition(name) => Some(name),
            _ => None,
        }
    }
}

/// One declared partition: a name and its glob set.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Partition {
    name: String,
    globs: Vec<String>,
}

/// The region map (ADR-0047 §6): a pure function of config + file path. Built
/// once at planning time; the salsa `Project`, index, and checker are untouched.
///
/// With no declared partitions and no observers this is the **single-region
/// identity**: every first-party file is [`RegionId::shared`] — the whole-universe
/// posture that makes a partitioned run byte-identical to today's behavior when
/// no `[transform.partitions]` section is present.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PartitionMap {
    /// Declared partitions, in deterministic (name-sorted) order. Disjoint by
    /// construction — [`PartitionMap::build`] rejects overlap.
    partitions: Vec<Partition>,
    /// Observer globs (ADR-0047 §1).
    observers: Vec<String>,
}

/// A partition config error (ADR-0047 §7): two partitions whose globs overlap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionConfigError {
    /// First partition name in the conflicting pair.
    pub partition_a: String,
    /// The glob from `partition_a` that overlaps.
    pub glob_a: String,
    /// Second partition name in the conflicting pair.
    pub partition_b: String,
    /// The glob from `partition_b` that overlaps.
    pub glob_b: String,
}

impl std::fmt::Display for PartitionConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "partitions `{}` and `{}` overlap: `{}` and `{}` can match the same path \
             (partition path-sets must be disjoint, ADR-0047 §7)",
            self.partition_a, self.partition_b, self.glob_a, self.glob_b
        )
    }
}

impl std::error::Error for PartitionConfigError {}

impl PartitionMap {
    /// The single-region identity: no partitions, no observers. Every first-party
    /// file is [`RegionId::shared`] — today's whole-universe posture (ADR-0047 §1).
    #[must_use]
    pub fn single_region() -> Self {
        Self::default()
    }

    /// Whether this is the single-region identity (no partitions, no observers).
    #[must_use]
    pub fn is_single_region(&self) -> bool {
        self.partitions.is_empty() && self.observers.is_empty()
    }

    /// The declared partition names, in deterministic order.
    #[must_use]
    pub fn partition_names(&self) -> Vec<&str> {
        self.partitions.iter().map(|p| p.name.as_str()).collect()
    }

    /// Build a map from declared partition sets (name → globs) and observer globs
    /// (ADR-0047 §7). Partitions are stored name-sorted for deterministic
    /// iteration; since they are disjoint, order never affects assignment.
    ///
    /// # Errors
    /// Returns [`PartitionConfigError`] when two partitions' globs overlap — the
    /// declared path-sets must be disjoint (ADR-0047 §7). Observer globs are *not*
    /// checked for overlap: an observer legitimately sits inside a partition tree
    /// (a test) and wins the assignment precedence.
    pub fn build(
        sets: impl IntoIterator<Item = (String, Vec<String>)>,
        observers: Vec<String>,
    ) -> Result<Self, PartitionConfigError> {
        let mut partitions: Vec<Partition> =
            sets.into_iter().map(|(name, globs)| Partition { name, globs }).collect();
        partitions.sort_by(|a, b| a.name.cmp(&b.name));

        // Pairwise overlap check across distinct partitions (ADR-0047 §7).
        for i in 0..partitions.len() {
            for j in (i + 1)..partitions.len() {
                for ga in &partitions[i].globs {
                    for gb in &partitions[j].globs {
                        if globs_overlap(ga, gb) {
                            return Err(PartitionConfigError {
                                partition_a: partitions[i].name.clone(),
                                glob_a: ga.clone(),
                                partition_b: partitions[j].name.clone(),
                                glob_b: gb.clone(),
                            });
                        }
                    }
                }
            }
        }

        Ok(Self { partitions, observers })
    }

    /// Assign `path` to its region (ADR-0047 §1). Pure; see the module doc for the
    /// precedence (vendor → observer → partition → shared).
    #[must_use]
    pub fn region_of(&self, path: &str) -> RegionId {
        // 1. Vendor always wins — a partition glob never claims `vendor/`.
        if is_vendor_path(path) {
            return RegionId::vendor();
        }
        // 2. Observer.
        if self.observers.iter().any(|g| glob_match(g, path)) {
            return RegionId::Observer;
        }
        // 3. Partition (disjoint, so at most one matches).
        for p in &self.partitions {
            if p.globs.iter().any(|g| glob_match(g, path)) {
                return RegionId::Partition(p.name.clone());
            }
        }
        // 4. Unclaimed first-party → shared.
        RegionId::shared()
    }
}

/// The literal `/`-segment prefix of a glob: the leading segments before the
/// first segment that contains a wildcard (`*`). `svc-a/**` → `["svc-a"]`;
/// `a/b/c.php` → `["a", "b", "c.php"]`; `**/x` → `[]`.
fn literal_prefix(glob: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for seg in glob.split('/') {
        if seg.contains('*') {
            break;
        }
        out.push(seg);
    }
    out
}

/// Whether two partition globs can match a common path (ADR-0047 §7 overlap).
/// Computed on the literal segment prefixes: they overlap when one prefix is a
/// segment-prefix of the other (equal counts as overlap). Exact for the
/// documented `dir/**` form; a wildcard-leading glob has an empty prefix and so
/// overlaps everything (conservative — such a pattern is ambiguous for disjoint
/// partitions).
fn globs_overlap(a: &str, b: &str) -> bool {
    let pa = literal_prefix(a);
    let pb = literal_prefix(b);
    let n = pa.len().min(pb.len());
    pa[..n] == pb[..n]
}

/// Directory-prefix glob match over `/`-separated paths (the repo's `exclude`
/// dialect). `*` matches a run of non-`/`; `**` matches any run including `/`.
/// Anchored at both ends.
fn glob_match(pattern: &str, path: &str) -> bool {
    matches_from(pattern.as_bytes(), path.as_bytes())
}

fn matches_from(mut pat: &[u8], mut text: &[u8]) -> bool {
    loop {
        match pat.first() {
            None => return text.is_empty(),
            Some(b'*') if pat.get(1) == Some(&b'*') => {
                // `**` (optionally followed by `/`): match the remainder at every
                // suffix of `text`, crossing `/` freely.
                let rest = if pat.get(2) == Some(&b'/') { &pat[3..] } else { &pat[2..] };
                if rest.is_empty() {
                    return true;
                }
                let mut i = 0;
                loop {
                    if matches_from(rest, &text[i..]) {
                        return true;
                    }
                    if i >= text.len() {
                        return false;
                    }
                    i += 1;
                }
            }
            Some(b'*') => {
                // Single `*`: match a run of non-`/` characters.
                let rest = &pat[1..];
                let mut i = 0;
                loop {
                    if matches_from(rest, &text[i..]) {
                        return true;
                    }
                    if i >= text.len() || text[i] == b'/' {
                        return false;
                    }
                    i += 1;
                }
            }
            Some(&c) => {
                if text.first() == Some(&c) {
                    pat = &pat[1..];
                    text = &text[1..];
                } else {
                    return false;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> PartitionMap {
        PartitionMap::build(
            [
                ("svc-a".to_owned(), vec!["svc-a.example/**".to_owned()]),
                ("svc-b".to_owned(), vec!["svc-b.example/**".to_owned()]),
                ("batch".to_owned(), vec!["batch/**".to_owned()]),
            ],
            vec!["tests/**".to_owned(), "dev-script/**".to_owned()],
        )
        .expect("disjoint partitions build")
    }

    #[test]
    fn assigns_partition_files() {
        assert_eq!(map().region_of("svc-a.example/src/Foo.php"), RegionId::Partition("svc-a".into()));
        assert_eq!(map().region_of("svc-b.example/Bar.php"), RegionId::Partition("svc-b".into()));
        assert_eq!(map().region_of("batch/Job.php"), RegionId::Partition("batch".into()));
    }

    #[test]
    fn assigns_observer_files() {
        assert_eq!(map().region_of("tests/FooTest.php"), RegionId::Observer);
        assert_eq!(map().region_of("dev-script/seed.php"), RegionId::Observer);
    }

    #[test]
    fn unclaimed_first_party_is_shared() {
        // No partition/observer glob covers a shared-lib file → Shared (not vendor).
        assert_eq!(map().region_of("lib/Support/Str.php"), RegionId::shared());
        assert_eq!(map().region_of("src/Kernel.php"), RegionId::Shared { vendor: false });
    }

    #[test]
    fn vendor_is_always_shared_with_flag() {
        assert_eq!(map().region_of("vendor/acme/pkg/A.php"), RegionId::vendor());
        assert_eq!(map().region_of("svc-a.example/vendor/x/Y.php"), RegionId::Shared { vendor: true });
    }

    #[test]
    fn vendor_beats_a_partition_claim() {
        // A partition glob that accidentally covers vendor/ must NOT claim it:
        // vendor is always shared (ADR-0047 §1/§5).
        let m = PartitionMap::build(
            [("svc-a".to_owned(), vec!["svc-a.example/**".to_owned()])],
            vec![],
        )
        .expect("build");
        let r = m.region_of("svc-a.example/vendor/dep/File.php");
        assert_eq!(r, RegionId::vendor());
        assert!(r.is_vendor() && r.is_shared());
        assert!(r.partition_name().is_none());
    }

    #[test]
    fn observer_beats_partition_when_both_match() {
        // A test file inside a service tree is an observer, not the service's code.
        let m = PartitionMap::build(
            [("svc-a".to_owned(), vec!["svc-a.example/**".to_owned()])],
            vec!["svc-a.example/tests/**".to_owned()],
        )
        .expect("build");
        assert_eq!(m.region_of("svc-a.example/tests/FooTest.php"), RegionId::Observer);
        assert_eq!(m.region_of("svc-a.example/src/Foo.php"), RegionId::Partition("svc-a".into()));
    }

    #[test]
    fn overlapping_partitions_are_a_config_error() {
        let err = PartitionMap::build(
            [
                ("outer".to_owned(), vec!["svc-a.example/**".to_owned()]),
                ("inner".to_owned(), vec!["svc-a.example/sub/**".to_owned()]),
            ],
            vec![],
        )
        .expect_err("nested partition globs overlap");
        // Names are surfaced (order is name-sorted: inner < outer).
        assert_eq!(err.partition_a, "inner");
        assert_eq!(err.partition_b, "outer");
    }

    #[test]
    fn identical_globs_in_two_partitions_overlap() {
        assert!(PartitionMap::build(
            [
                ("a".to_owned(), vec!["shared/**".to_owned()]),
                ("b".to_owned(), vec!["shared/**".to_owned()]),
            ],
            vec![],
        )
        .is_err());
    }

    #[test]
    fn sibling_partitions_do_not_overlap() {
        // Same ancestor, different subtrees — disjoint, must build.
        PartitionMap::build(
            [
                ("a".to_owned(), vec!["shared/a/**".to_owned()]),
                ("b".to_owned(), vec!["shared/b/**".to_owned()]),
            ],
            vec![],
        )
        .expect("siblings are disjoint");
    }

    #[test]
    fn wildcard_leading_glob_overlaps_conservatively() {
        // Empty literal prefix → treated as overlapping every partition.
        assert!(PartitionMap::build(
            [
                ("a".to_owned(), vec!["**/Generated.php".to_owned()]),
                ("b".to_owned(), vec!["svc-b/**".to_owned()]),
            ],
            vec![],
        )
        .is_err());
    }

    #[test]
    fn single_region_identity_assigns_everything_shared() {
        let m = PartitionMap::single_region();
        assert!(m.is_single_region());
        assert_eq!(m.region_of("anything/at/all.php"), RegionId::shared());
        assert_eq!(m.region_of("svc-a.example/Foo.php"), RegionId::shared());
        // Vendor is still vendor even in the identity map.
        assert_eq!(m.region_of("vendor/x/Y.php"), RegionId::vendor());
    }

    #[test]
    fn default_is_single_region() {
        assert!(PartitionMap::default().is_single_region());
        assert_eq!(PartitionMap::default(), PartitionMap::single_region());
    }

    #[test]
    fn glob_match_dialect() {
        assert!(glob_match("svc-a/**", "svc-a/deep/Foo.php"));
        assert!(!glob_match("svc-a/**", "svc-a.php")); // needs the `/`
        assert!(!glob_match("svc-a/**", "other/Foo.php"));
        assert!(glob_match("tests/*.php", "tests/a.php"));
        assert!(!glob_match("tests/*.php", "tests/deep/a.php")); // `*` stays in-segment
    }

    #[test]
    fn literal_prefix_stops_at_first_wildcard() {
        assert_eq!(literal_prefix("svc-a/**"), vec!["svc-a"]);
        assert_eq!(literal_prefix("a/b/c.php"), vec!["a", "b", "c.php"]);
        assert_eq!(literal_prefix("**/x"), Vec::<&str>::new());
        assert_eq!(literal_prefix("a/*/c"), vec!["a"]);
    }

    #[test]
    fn partition_names_are_sorted() {
        assert_eq!(map().partition_names(), vec!["batch", "svc-a", "svc-b"]);
    }
}
