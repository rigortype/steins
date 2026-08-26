//! The frozen-generation substrate (ADR-0092 §2): generation identity, the
//! payload-agnostic artifact container, and the candidate-then-publish store
//! under `<project>/.steins/`.
//!
//! A **generation** is one validated, immutable analysis of the universe
//! under one fingerprinted identity. This crate owns the correctness floor
//! everything else in the ADR-0092 series stands on, and nothing above it:
//!
//! - [`GenerationId`] and the fingerprint discipline ([`FieldHasher`],
//!   [`Fingerprint`]): blake3 over tagged, length-prefixed fields,
//!   domain-separated per kind — never `Hash` or serialization bytes.
//!   [`GenerationInputs`] fixes what identity covers: everything that can
//!   change a finding, nothing that only changes its rendering.
//! - The container ([`ArtifactBuilder`], [`ArtifactReader`]): one file per
//!   package, a section a named byte range behind a seekable directory.
//!   What section bytes *mean* belongs to the payload owners (#486–#489);
//!   here they are ranges.
//! - The store ([`Store`], [`Candidate`], [`Generation`]): a build writes a
//!   private candidate and publishes atomically; a half-written candidate is
//!   swept wholesale at the next open, never salvaged. An artifact another
//!   generation already holds byte for byte is *shared* rather than rewritten
//!   ([`share`], [`ShareKind`]) — a reflink where the filesystem offers one, a
//!   hard link otherwise, a copy as the floor.
//! - The sealed capture ([`SourceInventory`]): sources are captured once,
//!   sealed, and revalidated immediately before publish, so a concurrent
//!   edit rejects the whole candidate. The capture hands each file's bytes
//!   back as it hashes them ([`SourceInventory::capture_keeping`]), so an
//!   analysis reads its universe once and analyzes the bytes the fingerprint
//!   covers by construction.
//! - The partition vocabulary ([`Package`], [`PackageKind`],
//!   [`PackageUniverse`]): the Composer-package partition of the universe
//!   and its reverse-dependency closure (ADR-0092 §3). Plain data — the
//!   builder that reads a real `composer.lock` lives with project discovery
//!   in `steins-db`, which consumes this crate, never the reverse.
//!
//! The standing invariant, imported verbatim from the ADR: **a cache miss
//! may change cost, never meaning.** Artifacts carry [`SCHEMA_VERSION`]; a
//! mismatch is a [`Miss`], every decode failure is a [`Miss`], and every
//! [`Miss`] means rebuild-from-source — a cache, not an interchange format,
//! with no migration path by design.
//!
//! Deliberately not here: consumer wiring (the CLI stays cold until #487),
//! shard payload formats, residency policy.

mod container;
mod fingerprint;
mod identity;
mod inventory;
mod names;
mod partition;
pub mod share;
mod store;

pub use container::{
    ArtifactBuilder, ArtifactReader, DecodeBudget, DuplicateSection, Miss, SCHEMA_VERSION,
};
pub use fingerprint::{FieldHasher, Fingerprint};
pub use identity::{EnginePosture, GenerationId, GenerationInputs};
pub use inventory::{
    Captured, DriftKind, SourceDrift, SourceEntry, SourceError, SourceInventory,
};
pub use names::{NameError, PackageName, SectionName};
pub use partition::{Package, PackageKind, PackageUniverse};
pub use share::ShareKind;
pub use store::{Candidate, Generation, PublishError, Store};
