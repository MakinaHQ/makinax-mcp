//! Without `write` this crate cannot sign or send: there is no `exec`
//! module, so no `Executor` type, in a build without the feature.

/// Compile-level half: this file names nothing from `exec`, and in a build
/// without `write` it still compiles and runs.
#[cfg(not(feature = "write"))]
#[test]
fn no_executor_without_write() {
    const { assert!(!cfg!(feature = "write")) };
}

/// Source half, checked in both builds: the one declaration of `exec` sits
/// under `#[cfg(feature = "write")]`.
#[test]
fn the_exec_module_is_declared_only_under_write() {
    let lib = include_str!("../src/lib.rs");
    let lines: Vec<&str> = lib.lines().map(str::trim).collect();
    let decls: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains("mod exec"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(decls.len(), 1, "exactly one `mod exec` declaration");
    let at = decls[0];
    assert_eq!(lines[at], "pub mod exec;");
    assert!(
        at > 0 && lines[at - 1] == "#[cfg(feature = \"write\")]",
        "`pub mod exec;` must be directly under #[cfg(feature = \"write\")]"
    );
}
