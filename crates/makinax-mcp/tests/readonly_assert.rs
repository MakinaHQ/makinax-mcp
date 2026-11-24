//! `scripts/assert-readonly.sh` refuses a binary that carries any write
//! marker, or that does not identify as the read-only build, and passes a
//! clean read-only one. Fixture binaries stand in for real builds; CI runs
//! the script against the real read-only binary.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/assert-readonly.sh")
}

/// A shell script whose bytes contain `marker` and whose `--version` prints
/// `variant`.
fn fixture(dir: &Path, marker: &str, variant: &str) -> PathBuf {
    let p = dir.join("fixture-binary");
    std::fs::write(
        &p,
        format!("#!/bin/sh\n# {marker}\necho 'makinax-mcp 0.0.0 (fixture) [{variant}]'\n"),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
    p
}

fn passes(bin: &Path) -> (bool, String) {
    let out = Command::new("bash")
        .arg(script())
        .arg(bin)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn each_write_marker_fails_the_guard_and_a_clean_binary_passes() {
    let tmp = tempfile::tempdir().unwrap();
    for (marker, allowed) in [
        ("clean", true),
        ("cloudkms", false),
        ("kms.googleapis", false),
        ("keystore decrypt", false),
        ("Sends immediately. No simulation is required.", false),
        ("the configured signer could not be built", false),
        ("anvil_impersonateAccount", false),
    ] {
        let bin = fixture(tmp.path(), marker, "read-only");
        let (ok, stdout) = passes(&bin);
        assert_eq!(ok, allowed, "marker {marker}: {stdout}");
    }
}

#[test]
fn a_binary_that_is_not_the_read_only_build_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fixture(tmp.path(), "clean", "read-write");
    let (ok, stdout) = passes(&bin);
    assert!(!ok, "{stdout}");
    assert!(stdout.contains("identifies as"), "{stdout}");
}

/// `--live` passes a read-write binary carrying every marker, and fails one
/// missing any marker or identifying as the read-only build.
#[test]
fn live_mode_requires_every_marker_in_a_read_write_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let all = MARKERS_FOR_FIXTURE.join(" | ");
    let live = |bin: &Path| {
        let out = Command::new("bash")
            .arg(script())
            .arg("--live")
            .arg(bin)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };
    let (ok, stdout) = live(&fixture(tmp.path(), &all, "read-write"));
    assert!(ok, "{stdout}");
    for dropped in MARKERS_FOR_FIXTURE {
        let some: Vec<&str> = MARKERS_FOR_FIXTURE
            .iter()
            .copied()
            .filter(|m| *m != dropped)
            .collect();
        let (ok, stdout) = live(&fixture(tmp.path(), &some.join(" | "), "read-write"));
        assert!(!ok, "missing {dropped}: {stdout}");
        assert!(stdout.contains(dropped), "{stdout}");
    }
    let (ok, stdout) = live(&fixture(tmp.path(), &all, "read-only"));
    assert!(!ok, "{stdout}");
}

const MARKERS_FOR_FIXTURE: [&str; 6] = [
    "cloudkms",
    "kms.googleapis",
    "keystore decrypt",
    "Sends immediately. No simulation is required.",
    "the configured signer could not be built",
    "anvil_impersonateAccount",
];

/// The markers are real: each one is in the read-write server's own source
/// or makinax's, so a marker that stops existing cannot pass silently.
/// Source text is not the binary: CI's assert-readonly-binary job also runs
/// `assert-readonly.sh --live` on a real read-write build, which fails when
/// a marker is not linked into it.
#[test]
fn every_marker_names_text_the_write_build_contains() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = std::fs::read_to_string(script()).unwrap();
    let markers: Vec<&str> = script
        .split("MARKERS=(")
        .nth(1)
        .unwrap()
        .split(')')
        .next()
        .unwrap()
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('"'))
        .map(|l| l.trim_matches('"'))
        .collect();
    assert_eq!(
        markers, MARKERS_FOR_FIXTURE,
        "the fixtures test the script's own markers"
    );
    let mut sources = String::new();
    for dir in ["crates/makinax/src", "crates/makinax-mcp/src"] {
        collect(&repo.join(dir), &mut sources);
    }
    for m in &markers {
        // kms endpoints come from the GCP SDK, not this repository.
        if m.contains("kms") {
            continue;
        }
        assert!(sources.contains(m), "marker {m:?} is in no source file");
    }
}

fn collect(dir: &Path, out: &mut String) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push_str(&std::fs::read_to_string(&p).unwrap());
        }
    }
}
