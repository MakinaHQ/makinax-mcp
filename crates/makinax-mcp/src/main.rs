//! `makinax-mcp [--config <path>]`: serve the tools over stdio.
//!
//! `makinax-mcp --version` prints the build identity and variant;
//! `makinax-mcp tools` prints the tool list as JSON. Logs go to stderr:
//! stdout carries the MCP protocol and nothing else.

use std::path::PathBuf;

use makinax_mcp::{MakinaxService, config};
use rmcp::ServiceExt;
use tracing_subscriber::{EnvFilter, fmt};

const USAGE: &str = "usage: makinax-mcp [--config <path>]
       makinax-mcp tools
       makinax-mcp --version

Serves the makinax tools over stdio (MCP). The config path defaults to
$XDG_CONFIG_HOME/makinax/config.toml (~/.config/makinax/config.toml); the
MAKINAX_CONFIG env var overrides it, and MAKINAX__<SECTION>__<KEY> env vars
override single keys.";

enum Command {
    Serve { config: Option<PathBuf> },
    Tools,
    Version,
    Help,
}

fn parse_args(args: &[String]) -> Result<Command, String> {
    let mut config = None;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--version" | "-V" => return Ok(Command::Version),
            "--help" | "-h" => return Ok(Command::Help),
            "tools" => return Ok(Command::Tools),
            "--config" => {
                let p = args
                    .get(i + 1)
                    .ok_or_else(|| "--config needs a path".to_string())?;
                config = Some(PathBuf::from(p));
                i += 1;
            }
            _ => match a.strip_prefix("--config=") {
                Some(p) => config = Some(PathBuf::from(p)),
                None => return Err(format!("unknown argument `{a}`")),
            },
        }
        i += 1;
    }
    Ok(Command::Serve { config })
}

/// Take fd 1 for the protocol and point everything else that writes to
/// stdout at stderr, so a stray print from any dependency cannot corrupt
/// the wire. Returns the only route to the real stdout.
#[cfg(unix)]
fn claim_stdout() -> std::io::Result<std::fs::File> {
    use std::os::fd::FromRawFd;
    // SAFETY: fd 1 and 2 are open for the life of the process. `fcntl`
    // returns a fresh descriptor this function owns (checked below before
    // it is wrapped), and `dup2` replaces fd 1 atomically.
    unsafe {
        let real = libc::fcntl(libc::STDOUT_FILENO, libc::F_DUPFD_CLOEXEC, 3);
        if real < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let wire = std::fs::File::from_raw_fd(real);
        if libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(wire)
    }
}

/// The service for a config that cannot be used. Building it cannot fail
/// in practice (an empty config has no URL to refuse); if it does, exit.
async fn unavailable(problem: String) -> MakinaxService {
    match MakinaxService::unavailable(problem).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "could not start even without a config");
            std::process::exit(1);
        }
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config_flag = match parse_args(&args) {
        Ok(Command::Version) => {
            println!("{}", makinax_mcp::version_line());
            return;
        }
        Ok(Command::Help) => {
            println!("{USAGE}");
            return;
        }
        Ok(Command::Tools) => {
            let tools = MakinaxService::router().list_all();
            match serde_json::to_string_pretty(&tools) {
                Ok(s) => println!("{s}"),
                Err(e) => {
                    eprintln!("makinax-mcp: could not render the tool list: {e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Ok(Command::Serve { config }) => config,
        Err(e) => {
            eprintln!("makinax-mcp: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    #[cfg(unix)]
    let wire = match claim_stdout() {
        Ok(f) => tokio::fs::File::from_std(f),
        Err(e) => {
            eprintln!("makinax-mcp: could not take stdout for the protocol: {e}");
            std::process::exit(1);
        }
    };
    #[cfg(not(unix))]
    let wire = tokio::io::stdout();

    // Positive control for tests/stdio_wire.rs: a stray print after the
    // claim must land on stderr. Debug builds only; a release binary has no
    // such hook.
    #[cfg(debug_assertions)]
    if std::env::var_os("MAKINAX_MCP_TEST_STRAY_STDOUT").is_some() {
        println!("STRAY-NOT-JSON");
    }

    // A config problem never stops the server: it serves a service whose
    // config-reading tools return the problem, so the client can read why.
    let (path, origin) = config::resolve_path(config_flag);
    let service = match config::load(&path, origin) {
        Ok(loaded) => {
            if loaded.source.ends_with("(file not found)") {
                tracing::warn!(config = %loaded.source, "no config file; serving with an empty config");
            }
            match MakinaxService::with_source(loaded.config, loaded.source).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(error = %e, "could not start with this config");
                    unavailable(e.to_string()).await
                }
            }
        }
        Err(problem) => {
            tracing::error!(error = %problem, "could not read the config");
            unavailable(problem).await
        }
    };

    let handle = service.clone();
    let result = match service.serve((tokio::io::stdin(), wire)).await {
        Ok(running) => running
            .waiting()
            .await
            .map(|_| ())
            .map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    handle.shutdown();
    if let Err(e) = result {
        tracing::error!(error = %e, "server stopped");
        std::process::exit(1);
    }
}
