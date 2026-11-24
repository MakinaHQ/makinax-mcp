//! Where the config file is, and reading it into a [`MakinaxConfig`].
//!
//! Precedence: `--config <path>`, then `MAKINAX_CONFIG`, then
//! `$XDG_CONFIG_HOME/makinax/config.toml` (`~/.config/makinax/config.toml`
//! when `XDG_CONFIG_HOME` is unset). Environment variables prefixed
//! `MAKINAX__` override single keys, with `__` between sections (for example
//! `MAKINAX__RPC_URLS__BASE`).

use std::path::{Path, PathBuf};

use makinax::ext::MakinaxConfig;

/// The env var naming the config file.
pub const CONFIG_ENV: &str = "MAKINAX_CONFIG";

/// How the config path was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathOrigin {
    /// `--config <path>`.
    Flag,
    /// The `MAKINAX_CONFIG` env var.
    Env,
    /// The default location.
    Default,
}

/// The config path to read, and how it was chosen.
pub fn resolve_path(flag: Option<PathBuf>) -> (PathBuf, PathOrigin) {
    if let Some(p) = flag {
        return (p, PathOrigin::Flag);
    }
    if let Some(p) = std::env::var_os(CONFIG_ENV).filter(|v| !v.is_empty()) {
        return (PathBuf::from(p), PathOrigin::Env);
    }
    (default_path(), PathOrigin::Default)
}

/// `$XDG_CONFIG_HOME/makinax/config.toml`, or `~/.config/makinax/config.toml`.
pub fn default_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        })
        .join("makinax")
        .join("config.toml")
}

/// A config that loaded, with a note for the reader when the file was absent.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: MakinaxConfig,
    /// Where the config came from, for error messages: the path, and
    /// whether the file existed.
    pub source: String,
}

/// Read the config at `path`, with `MAKINAX__` env overrides.
///
/// A path named explicitly (flag or env var) must exist. A missing file at
/// the default location is an empty config: the server still starts, and
/// every module tool names the file it looked for.
pub fn load(path: &Path, origin: PathOrigin) -> Result<Loaded, String> {
    let exists = path.exists();
    if !exists && origin != PathOrigin::Default {
        let how = match origin {
            PathOrigin::Flag => "--config",
            _ => CONFIG_ENV,
        };
        return Err(format!(
            "no config file at {} (named by {how})",
            path.display()
        ));
    }
    let raw = ::config::Config::builder()
        .add_source(::config::File::from(path).required(false))
        .add_source(
            ::config::Environment::with_prefix("MAKINAX")
                .prefix_separator("__")
                .separator("__")
                .try_parsing(true),
        )
        .build()
        .map_err(|e| config_error(path, e))?;
    let config: MakinaxConfig = raw.try_deserialize().map_err(|e| config_error(path, e))?;
    let source = if exists {
        path.display().to_string()
    } else {
        format!("{} (file not found)", path.display())
    };
    Ok(Loaded { config, source })
}

/// A config that could not be read or parsed. The parser quotes the value
/// it could not use, and an RPC URL carries its key, so the text is masked:
/// it is logged, and every tool returns it.
fn config_error(path: &Path, e: impl std::fmt::Display) -> String {
    makinax::ext::redact::text(&format!("config {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_path_that_does_not_exist_is_an_error_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("absent.toml");
        let e = load(&p, PathOrigin::Flag).unwrap_err();
        assert!(e.contains("absent.toml") && e.contains("--config"), "{e}");
    }

    #[test]
    fn a_missing_default_file_is_an_empty_config() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("absent.toml");
        let l = load(&p, PathOrigin::Default).unwrap();
        assert!(l.config.modules.is_empty());
        assert!(l.source.contains("file not found"), "{}", l.source);
    }

    #[test]
    fn a_file_parses_into_the_makinax_config() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(
            &p,
            r#"
[rpc_urls]
base = "http://127.0.0.1:1"

[modules.main]
chain = "base"
safe = "0x1111111111111111111111111111111111111111"
module = "0x2222222222222222222222222222222222222222"
"#,
        )
        .unwrap();
        let l = load(&p, PathOrigin::Flag).unwrap();
        assert_eq!(l.config.modules["main"].chain, "base");
        assert!(l.config.rpc_urls.contains_key("base"));
    }

    #[test]
    fn a_malformed_file_is_an_error_naming_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "[modules.main]\nchain = 1\n").unwrap();
        let e = load(&p, PathOrigin::Flag).unwrap_err();
        assert!(e.contains("config.toml"), "{e}");
    }

    /// A parse error quotes the value it could not use. When that value is
    /// a keyed URL, the key is masked in the error.
    #[test]
    fn a_parse_error_that_quotes_a_keyed_url_masks_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(
            &p,
            "[events]\nchunk_blocks = \"https://rpc.example.com/v2/9fKd2Lm4Xp7Qw1Nz8Bt3Yr\"\n",
        )
        .unwrap();
        let e = load(&p, PathOrigin::Flag).unwrap_err();
        assert!(e.contains("config.toml"), "{e}");
        assert!(e.contains("rpc.example.com"), "the value is quoted: {e}");
        assert!(!e.contains("9fKd2Lm4Xp7Qw1Nz8Bt3Yr"), "{e}");
    }
}
