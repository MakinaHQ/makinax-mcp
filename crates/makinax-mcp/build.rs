fn main() {
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    // `--no-optional-locks`: PLAIN `git status` REWRITES `.git/index` whenever
    // the stat cache is stale, which any file touch makes it. This script
    // WATCHES `.git/index` — so the dirty check was invalidating the very
    // fingerprint it is part of, and the crate rebuilt once more after every
    // working-tree change even in a normal checkout. Measured on a minimal
    // repro with this exact script: three no-change builds recompile once
    // without the flag and never with it. The flag only stops git WRITING the
    // refreshed index; it detects a dirty tree identically (verified against
    // plain `status` on the same clean and dirty trees).
    let dirty = std::process::Command::new("git")
        .args([
            "--no-optional-locks",
            "status",
            "--porcelain",
            "--untracked-files=no",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    println!(
        "cargo:rustc-env=MAKINAX_GIT_SHA={}{}",
        sha,
        if dirty { "-dirty" } else { "" }
    );
    // RESOLVE THE REAL GIT DIRECTORY. In a git WORKTREE, `.git` is a FILE
    // pointing at the gitdir, so `.git/HEAD` and `.git/index` do not exist as
    // paths — and cargo treats a missing `rerun-if-changed` target as ALWAYS
    // DIRTY. That re-runs this script on every invocation, which recompiles
    // the whole crate on every invocation, in every worktree, forever.
    //
    // `--absolute-git-dir`, NOT `--git-common-dir`: a worktree's HEAD and
    // index live in its own `.git/worktrees/<name>/`, and the common dir holds
    // the MAIN checkout's. Watching the common dir would fix the rebuild loop
    // and then miss a branch switch in the worktree — a stale embedded SHA
    // traded for speed, which is a worse bug than the one being fixed. In a
    // normal checkout both forms answer `.git`.
    let git_dir = std::process::Command::new("git")
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| ".git".into());
    //
    // EMITTING NOTHING IS THE SAFE FALLBACK, not a hole: with no
    // `rerun-if-changed` at all, cargo re-runs a build script whenever any
    // file in the package changes. So an unpacked source tarball with no
    // repository (where the SHA is already "unknown") stays correct and
    // merely rebuilds a little more eagerly. Never stale, at worst keen.
    for f in ["HEAD", "index"] {
        let p = std::path::Path::new(&git_dir).join(f);
        if p.exists() {
            println!("cargo:rerun-if-changed={}", p.display());
        }
    }
}
