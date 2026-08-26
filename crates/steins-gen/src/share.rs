//! Giving a new generation an artifact that already exists, without rewriting
//! its bytes (issue #519).
//!
//! A generation is a directory of artifacts, and most of them are the previous
//! generation's byte for byte: an edit touches one package, and every other
//! package's artifact — all of vendor, in the shape ADR-0092 §3 is built for —
//! is republished unchanged. Copying those bytes is the cost of typing a
//! character, and it buys nothing, because the two files are equal.
//!
//! Three mechanisms, in falling order of preference; the caller learns which
//! one served, and nothing above this module depends on the answer:
//!
//! 1. **Reflink** — a copy-on-write clone: a *distinct* inode that shares the
//!    source's extents until one side is written. macOS `clonefile(2)` on
//!    APFS, Linux `FICLONE` on btrfs/XFS. No aliasing exists at all.
//! 2. **Hard link** — a second directory entry for the same inode, on the
//!    filesystems that have no clone (ext4 today, and CI's).
//! 3. **Copy** — the floor, always correct and always available.
//!
//! **The mutation argument.** A hard link is an alias, and an alias is only
//! safe if nothing can ever write through it. Two properties make that true
//! here, and the first is structural rather than a promise:
//!
//! * The only writer of an artifact is [`ArtifactBuilder::write_to`], and it
//!   opens with `O_CREAT | O_EXCL` ([`std::fs::File::create_new`]). A write
//!   aimed at a name that already exists — adopted or not — fails loudly
//!   instead of truncating whatever inode is behind it. There is no path in
//!   this crate that opens an artifact for writing any other way, and a
//!   published artifact is only ever opened read-only ([`ArtifactReader`]).
//! * A candidate writes each package at most once, into a directory it created
//!   itself moments earlier, so an adopted name never collides with a written
//!   one within a generation either.
//!
//! Unlinking is therefore the only thing that ever happens to a shared inode:
//! `remove_dir_all` over one generation drops one directory entry, and the
//! other generation's entry keeps the bytes alive. Removing a generation can
//! never damage another one.
//!
//! **Every failure leaves nothing behind.** [`share`] guarantees that on `Err`
//! the destination does not exist, so the caller can fall through to building
//! the artifact from scratch at the same path.
//!
//! [`ArtifactBuilder::write_to`]: crate::ArtifactBuilder::write_to
//! [`ArtifactReader`]: crate::ArtifactReader

use std::fs::{self, File};
use std::io;
use std::path::Path;

/// How an artifact reached a candidate. Reported for the run's notes and the
/// perf harness; no decision anywhere depends on it, because all three
/// mechanisms leave the destination holding exactly the source's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareKind {
    /// A copy-on-write clone — a distinct inode sharing extents.
    Reflink,
    /// A second directory entry for the same inode.
    HardLink,
    /// A byte-for-byte copy.
    Copy,
}

impl ShareKind {
    /// The word for a note: `"reflinked"`, `"hard-linked"`, `"copied"`.
    #[must_use]
    pub fn verb(self) -> &'static str {
        match self {
            ShareKind::Reflink => "reflinked",
            ShareKind::HardLink => "hard-linked",
            ShareKind::Copy => "copied",
        }
    }
}

/// Put the contents of `src` at `dst`, as cheaply as the filesystem allows.
///
/// `dst` must not exist; if it does, this is [`io::ErrorKind::AlreadyExists`]
/// and nothing is touched. On any error `dst` is left absent, so a caller that
/// falls back to writing the artifact itself starts from a clean name.
pub fn share(src: &Path, dst: &Path) -> io::Result<ShareKind> {
    if reflink(src, dst).is_ok() {
        return Ok(ShareKind::Reflink);
    }
    if fs::hard_link(src, dst).is_ok() {
        return Ok(ShareKind::HardLink);
    }
    copy(src, dst).map(|()| ShareKind::Copy)
}

/// The floor. `create_new` rather than [`fs::copy`] for the same reason
/// [`ArtifactBuilder::write_to`] uses it: a copy must never be able to
/// truncate a name that is already there.
///
/// [`ArtifactBuilder::write_to`]: crate::ArtifactBuilder::write_to
fn copy(src: &Path, dst: &Path) -> io::Result<()> {
    let mut source = File::open(src)?;
    let mut target = File::create_new(dst)?;
    match io::copy(&mut source, &mut target) {
        Ok(_) => Ok(()),
        Err(e) => {
            drop(target);
            let _ = fs::remove_file(dst);
            Err(e)
        }
    }
}

/// macOS: `clonefile(2)`. APFS clones; every other filesystem refuses, which
/// is an ordinary `Err` here and the hard link next.
#[cfg(target_os = "macos")]
fn reflink(src: &Path, dst: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let invalid = || io::Error::from(io::ErrorKind::InvalidInput);
    let src = CString::new(src.as_os_str().as_bytes()).map_err(|_| invalid())?;
    let dst = CString::new(dst.as_os_str().as_bytes()).map_err(|_| invalid())?;
    // SAFETY: both arguments are NUL-terminated C strings owned for the whole
    // call, and `clonefile` reads them and nothing else. It creates `dst` or
    // fails; it never writes through an existing name.
    let rc = unsafe { libc::clonefile(src.as_ptr(), dst.as_ptr(), 0) };
    if rc == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// Linux: `FICLONE`, which needs the destination to exist first — so a failure
/// has to take the empty file it created back out, or the caller's fallback
/// would find the name occupied.
#[cfg(target_os = "linux")]
fn reflink(src: &Path, dst: &Path) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let source = File::open(src)?;
    let target = File::create_new(dst)?;
    // SAFETY: `FICLONE` takes exactly one argument, an open file descriptor to
    // clone from; both descriptors are live for the duration of the call.
    let rc = unsafe { libc::ioctl(target.as_raw_fd(), libc::FICLONE, source.as_raw_fd()) };
    if rc == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    drop(target);
    let _ = fs::remove_file(dst);
    Err(error)
}

/// Everywhere else: no clone syscall to try, so the hard link is the first
/// mechanism and the copy is the floor.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn reflink(_src: &Path, _dst: &Path) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "steins-gen-share-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn whatever_the_mechanism_the_bytes_arrive() {
        let dir = scratch("bytes");
        let src = dir.join("src");
        fs::write(&src, b"the payload").unwrap();
        let dst = dir.join("dst");
        share(&src, &dst).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"the payload");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Removing one name never disturbs the other, whichever mechanism served.
    #[test]
    fn removing_one_side_leaves_the_other_readable() {
        let dir = scratch("unlink");
        let src = dir.join("src");
        fs::write(&src, b"shared bytes").unwrap();
        let dst = dir.join("dst");
        share(&src, &dst).unwrap();
        fs::remove_file(&src).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"shared bytes");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_occupied_destination_is_refused_and_left_alone() {
        let dir = scratch("occupied");
        let src = dir.join("src");
        fs::write(&src, b"new").unwrap();
        let dst = dir.join("dst");
        fs::write(&dst, b"old").unwrap();
        let err = share(&src, &dst).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&dst).unwrap(), b"old");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_absent_source_leaves_no_destination_behind() {
        let dir = scratch("absent");
        let dst = dir.join("dst");
        assert!(share(&dir.join("nothing"), &dst).is_err());
        assert!(!dst.exists(), "a failed share leaves the name free for the fallback");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// One rung of [`share`]'s fallback chain, as a plain function.
    type Rung = fn(&Path, &Path) -> io::Result<()>;

    /// Each rung on its own. [`share`] takes whichever the filesystem under
    /// the test happens to offer — APFS clones, ext4 hard-links — so the two
    /// it does *not* take would otherwise never run anywhere.
    #[test]
    fn every_rung_delivers_the_bytes_and_refuses_an_occupied_name() {
        let dir = scratch("rungs");
        let src = dir.join("src");
        fs::write(&src, b"rung payload").unwrap();
        let rungs: [(&str, Rung); 3] =
            [("reflink", reflink), ("hardlink", |s, d| fs::hard_link(s, d)), ("copy", copy)];
        for (name, rung) in rungs {
            let dst = dir.join(name);
            // A rung the filesystem does not offer refuses cleanly; one it does
            // must deliver the bytes and then refuse the occupied name.
            if rung(&src, &dst).is_err() {
                assert!(!dst.exists(), "{name}: a refused rung leaves no destination");
                continue;
            }
            assert_eq!(fs::read(&dst).unwrap(), b"rung payload", "{name}");
            assert_eq!(
                rung(&src, &dst).unwrap_err().kind(),
                io::ErrorKind::AlreadyExists,
                "{name}: an occupied name is never written through"
            );
            assert_eq!(fs::read(&dst).unwrap(), b"rung payload", "{name}");
        }
        fs::remove_dir_all(&dir).unwrap();
    }
}
