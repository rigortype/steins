//! End-to-end tests for `steins version` and `steins license` (issue #43).
//!
//! Both carry a legal obligation rather than a convenience: the release archive
//! ships `LICENSE` and `THIRD-PARTY-LICENSES.md` beside the binary, but nothing
//! downstream keeps them together — the Homebrew formula installs the executable
//! and the third-party notices and *not* `LICENSE`, and `cargo install --git`
//! produces a bare binary. So the notices are embedded, and these tests check
//! that what is embedded is the real thing rather than a stub.

use std::process::{Command, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_steins")
}

fn run(args: &[&str]) -> (i32, String) {
    let out = Command::new(bin()).args(args).output().expect("run steins");
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn version_names_the_build_and_points_at_the_license_command() {
    for form in [["version"], ["--version"], ["-v"]] {
        let (code, out) = run(&form);
        assert_eq!(code, 0, "`steins {}` must succeed", form[0]);
        assert!(out.starts_with("steins "), "{form:?}: {out}");
        assert!(out.contains(env!("CARGO_PKG_VERSION")), "{form:?}");
        assert!(out.contains("revision "), "the build stamp is missing: {out}");
        assert!(out.contains("Copyright (c) TypedDuck"), "the notice must name the holder README states");
        assert!(out.contains("steins license"), "must point at the license command");
    }
}

#[test]
fn license_carries_our_terms_and_the_bundled_notices() {
    let (code, out) = run(&["license"]);
    assert_eq!(code, 0);

    // Steins' own terms — Apache-2.0 §4(a) requires a recipient to get a copy.
    assert!(out.contains("Apache License"), "our own license text is missing");
    assert!(out.contains("TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION"));

    // The dependencies' notices, whose MIT/BSD/ISC terms require them to
    // accompany a binary distribution.
    assert!(out.contains("Third-Party Licenses"));
    assert!(out.contains("Permission is hereby granted"), "an MIT body must be present");

    // `licenses` is the same command.
    let (code2, out2) = run(&["licenses"]);
    assert_eq!(code2, 0);
    assert_eq!(out, out2);
}

#[test]
fn the_embedded_notices_are_generated_not_a_stub() {
    // Guards the failure this whole surface exists to prevent: a placeholder or
    // truncated notices file that makes the command look compliant while
    // carrying nothing. Assert real bodies and a real dependency, never merely
    // that the output is non-empty.
    let (_, out) = run(&["license"]);
    assert!(out.matches("\n## ").count() >= 8, "expected the licence sections");
    assert!(out.contains("Used by:"));
    assert!(out.contains("— https://"), "entries must carry crate repositories");
    // The evidence moved with issue #45: sections are now grouped by permission
    // notice, so there are 9 of them rather than 44 and a section count no longer
    // measures much. What a stub cannot fake is the *attribution* — thirty-odd
    // distinct copyright notices, and these named holders among them.
    let notices = out.lines().filter(|l| l.starts_with("Copyright")).count();
    assert!(notices >= 30, "the bundled copyright notices are missing (found {notices})");
    for holder in ["Copyright (c) 2014 Alex Crichton", "Copyright (c) 2010 The Rust Project Developers"] {
        assert!(out.contains(holder), "missing `{holder}`");
    }
}

#[test]
fn typographic_variants_are_merged_into_one_section() {
    // Issue #43: cargo-about groups on exact license text, so Apache-2.0 shipped
    // centred by one crate and flush-left by another produced two sections of
    // the same name. `cargo xtask licenses` merges them; this pins that the
    // committed file is the merged one.
    let (_, out) = run(&["license"]);
    let third_party = out.split("Third-Party Licenses").nth(1).expect("third-party section");
    assert_eq!(
        third_party.matches("\n## Apache License 2.0").count(),
        1,
        "Apache-2.0 must appear as exactly one entry among the dependencies"
    );
    assert_eq!(third_party.matches("\n## ISC License").count(), 1, "ISC likewise");
}

#[test]
fn mit_is_one_section_carrying_every_holder() {
    // Issue #45: 39 generated MIT sections share one permission notice, so they
    // are one section with every crate's copyright notice listed above the grant.
    // The embedded copy is what a `brew install` user reads, so the property is
    // checked on the command's output and not only on the file.
    let (_, out) = run(&["license"]);
    let third_party = out.split("Third-Party Licenses").nth(1).expect("third-party section");
    assert_eq!(third_party.matches("\n## MIT License").count(), 1, "MIT is one section");
    let mit = third_party
        .split("\n## MIT License")
        .nth(1)
        .and_then(|s| s.split("\n## ").next())
        .expect("the MIT section");
    assert_eq!(
        mit.matches("Permission is hereby granted").count(),
        1,
        "one permission notice for the group"
    );
    let holders = mit.lines().filter(|l| l.starts_with("Copyright")).count();
    assert!(holders >= 30, "the grouped section must keep every holder (found {holders})");
}

#[test]
fn a_closed_pipe_is_not_a_crash() {
    // `steins license` emits thousands of lines, so `| head` and quitting `less`
    // early are the normal ways to read it. `println!` panics on EPIPE; this
    // pins that the command does not.
    let mut child = Command::new(bin())
        .arg("license")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn steins");
    // Drop the read end immediately, then wait: the writer sees EPIPE.
    drop(child.stdout.take());
    let out = child.wait_with_output().expect("wait");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panicked"), "closed pipe must not panic: {stderr}");
    assert!(out.status.success(), "closed pipe must still exit 0");
}
