//! This server is neutral: it never names a consumer or a product.
//! Scans every file in this crate (src, tests, Cargo.toml, build script,
//! README and skill).
use std::path::{Path, PathBuf};

// Spelled in pieces so this file does not itself contain the words.
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

#[test]
fn this_crate_never_names_a_consumer_or_product() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut all = Vec::new();
    files(root, &mut all);
    let mut offenders = Vec::new();
    for f in &all {
        let Ok(text) = std::fs::read_to_string(f) else {
            continue;
        };
        let lower = text.to_lowercase();
        for word in FORBIDDEN {
            if let Some(i) = lower.find(word) {
                let line = lower[..i].matches('\n').count() + 1;
                offenders.push(format!("{}:{line}: {word}", f.display()));
            }
        }
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
    for f in [
        "src/lib.rs",
        "src/tools/exec.rs",
        "Cargo.toml",
        "README.md",
        "skills/makinax/SKILL.md",
    ] {
        assert!(all.iter().any(|p| p.ends_with(f)), "guard missed {f}");
    }
}
