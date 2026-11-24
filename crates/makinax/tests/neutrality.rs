//! makinax is neutral: it never names a consumer or a product.
//! Scans every file in this crate (src, tests, Cargo.toml), and every file
//! tracked in the repository around it.
use std::path::{Path, PathBuf};

// Spelled in pieces so this file does not itself contain the words, and a
// plain `git grep` over the repository comes back empty too.
const FORBIDDEN: &[&str] = &[concat!("orb", "it"), concat!("dia", "lectic")];

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            files(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// Each forbidden word in `f`'s path or contents, as `path:line: word`.
fn violations(f: &Path, shown: &str, offenders: &mut Vec<String>) {
    let name = shown.to_lowercase();
    for word in FORBIDDEN {
        if name.contains(word) {
            offenders.push(format!("{shown}: {word} (in the path)"));
        }
    }
    let Ok(bytes) = std::fs::read(f) else {
        return;
    };
    let lower = String::from_utf8_lossy(&bytes).to_lowercase();
    for word in FORBIDDEN {
        if let Some(i) = lower.find(word) {
            let line = lower[..i].matches('\n').count() + 1;
            offenders.push(format!("{shown}:{line}: {word}"));
        }
    }
}

#[test]
fn makinax_never_names_a_consumer_or_product() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut all = Vec::new();
    files(root, &mut all);
    let mut offenders = Vec::new();
    for f in &all {
        violations(f, &f.display().to_string(), &mut offenders);
    }
    assert!(
        offenders.is_empty(),
        "neutrality violations:\n{}",
        offenders.join("\n")
    );
}

/// The files git tracks in the repository that holds this crate: the root
/// manifest, workflows, scripts, packaging and docs, as well as both crates.
fn tracked_files() -> (PathBuf, Vec<String>) {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["ls-files", "-z"])
        .output()
        .expect("git must be on PATH to scan the repository");
    assert!(
        out.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let names = String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    (repo, names)
}

#[test]
fn the_repository_never_names_a_consumer_or_product() {
    let (repo, names) = tracked_files();
    let mut offenders = Vec::new();
    for name in &names {
        violations(&repo.join(name), name, &mut offenders);
    }
    assert!(
        offenders.is_empty(),
        "neutrality violations:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn the_guard_sees_files() {
    let mut all = Vec::new();
    files(Path::new(env!("CARGO_MANIFEST_DIR")), &mut all);
    assert!(
        all.iter().any(|f| f.ends_with("src/lib.rs")),
        "guard walked nothing"
    );
    let (_, names) = tracked_files();
    for f in [
        "Cargo.toml",
        "README.md",
        ".github/workflows/release.yml",
        "packaging/homebrew/makinax-mcp.rb",
        "scripts/assert-readonly.sh",
        "crates/makinax-mcp/src/lib.rs",
    ] {
        assert!(names.iter().any(|n| n == f), "repository guard missed {f}");
    }
}

/// The scan is not vacuous: a planted word is reported with its line.
#[test]
fn the_guard_reports_a_planted_word() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("planted.md");
    std::fs::write(
        &f,
        format!("first\nsecond {}\n", FORBIDDEN[0].to_uppercase()),
    )
    .unwrap();
    let mut offenders = Vec::new();
    violations(&f, "planted.md", &mut offenders);
    assert_eq!(offenders, vec![format!("planted.md:2: {}", FORBIDDEN[0])]);
}
