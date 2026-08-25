//! The naming vocabulary later slices import: packages and sections. Both are
//! validated at construction so the store and the container never meet a name
//! they cannot spell on disk or in a directory entry.

use std::fmt;

/// A name was rejected at construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameError {
    what: &'static str,
    why: &'static str,
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid {}: {}", self.what, self.why)
    }
}

impl std::error::Error for NameError {}

/// The unit of persistence (ADR-0092 §3): a Composer package name
/// (`vendor/name`), or whatever name the builder gives the first-party
/// partition(s). Structurally: 1–200 bytes of printable ASCII, no whitespace.
/// The store derives the artifact file name from this by percent-encoding
/// everything outside `[a-z0-9._-]`, so any valid `PackageName` is spellable
/// on any filesystem, case-collision-free.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageName(String);

impl PackageName {
    pub fn new(name: &str) -> Result<Self, NameError> {
        let err = |why| Err(NameError { what: "package name", why });
        if name.is_empty() {
            return err("empty");
        }
        if name.len() > 200 {
            return err("longer than 200 bytes");
        }
        if !name.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
            return err("bytes outside printable ASCII (or whitespace)");
        }
        Ok(Self(name.to_owned()))
    }

    pub fn as_str(&self) -> &str { &self.0 }
}

impl fmt::Display for PackageName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}

/// A named byte range in an artifact container. The directory stores names in
/// a fixed-width field, so a name is 1–16 bytes of `[a-z0-9_-]` — the fixed
/// vocabulary each payload owner (#486–#489) declares as constants.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SectionName(String);

impl SectionName {
    /// The directory's fixed name-field width.
    pub const MAX_LEN: usize = 16;

    pub fn new(name: &str) -> Result<Self, NameError> {
        let err = |why| Err(NameError { what: "section name", why });
        if name.is_empty() {
            return err("empty");
        }
        if name.len() > Self::MAX_LEN {
            return err("longer than 16 bytes");
        }
        if !name.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-')) {
            return err("bytes outside [a-z0-9_-]");
        }
        Ok(Self(name.to_owned()))
    }

    pub fn as_str(&self) -> &str { &self.0 }
}

impl fmt::Display for SectionName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_name_validation() {
        assert!(PackageName::new("vendor/name").is_ok());
        assert!(PackageName::new("__first_party__").is_ok());
        assert!(PackageName::new("").is_err());
        assert!(PackageName::new("has space").is_err());
        assert!(PackageName::new("tab\there").is_err());
        assert!(PackageName::new("newline\n").is_err());
        assert!(PackageName::new(&"a".repeat(201)).is_err());
        assert!(PackageName::new("naïve/name").is_err());
    }

    #[test]
    fn section_name_validation() {
        assert!(SectionName::new("symbols").is_ok());
        assert!(SectionName::new("a").is_ok());
        assert!(SectionName::new(&"a".repeat(16)).is_ok());
        assert!(SectionName::new(&"a".repeat(17)).is_err());
        assert!(SectionName::new("").is_err());
        assert!(SectionName::new("UPPER").is_err());
        assert!(SectionName::new("dotted.name").is_err());
        assert!(SectionName::new("sl/ash").is_err());
    }
}
