//! The tool set is final and named; the read-only build has no way to send
//! (review focus 4); every description is neutral.

use std::collections::BTreeSet;

use makinax_mcp::MakinaxService;

const READ_CLASS: [&str; 18] = [
    // read
    "list_modules",
    "get_module_state",
    "get_balances",
    "get_oracle_price",
    "get_module_events",
    "list_actions",
    // proof
    "get_proof",
    "check_root",
    // build
    "build_call",
    "preview_call",
    // owner
    "list_owner_functions",
    "encode_owner_call",
    "build_owner_batch",
    // fetch
    "get_swap_quote",
    "get_bridge_quote",
    "get_cctp_attestation",
    // sim
    "simulate_call",
    "simulate_owner_batch",
];

fn names() -> BTreeSet<String> {
    MakinaxService::router()
        .list_all()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect()
}

#[cfg(feature = "write")]
#[test]
fn the_read_write_build_serves_exactly_the_nineteen_tools() {
    let mut want: BTreeSet<String> = READ_CLASS.iter().map(|s| s.to_string()).collect();
    want.insert("execute_call".to_string());
    assert_eq!(want.len(), 19);
    assert_eq!(names(), want);
}

#[cfg(not(feature = "write"))]
#[test]
fn the_read_only_build_serves_the_eighteen_and_no_execute() {
    let want: BTreeSet<String> = READ_CLASS.iter().map(|s| s.to_string()).collect();
    let got = names();
    assert_eq!(got.len(), 18);
    assert_eq!(got, want);
    assert!(!got.contains("execute_call"), "{got:?}");
}

#[cfg(feature = "write")]
#[test]
fn execute_call_says_it_sends_immediately_and_coordinates_with_nothing() {
    let tools = MakinaxService::router().list_all();
    let exec = tools
        .iter()
        .find(|t| t.name == "execute_call")
        .expect("execute_call");
    let d = exec.description.as_deref().unwrap_or_default();
    assert!(
        d.contains(
            "Sends immediately. No simulation is required. One sender per key: this server does \
             not coordinate with other processes using the same key."
        ),
        "{d}"
    );
    let ann = exec.annotations.as_ref().expect("annotated");
    assert_eq!(ann.read_only_hint, Some(false));
    assert_eq!(ann.destructive_hint, Some(true));
}

#[test]
fn every_read_class_tool_is_marked_read_only() {
    for t in MakinaxService::router().list_all() {
        if t.name == "execute_call" {
            continue;
        }
        let ro = t.annotations.as_ref().and_then(|a| a.read_only_hint);
        assert_eq!(ro, Some(true), "{} is not marked read-only", t.name);
    }
}

/// Product concepts. None belongs in a tool name, description or input
/// schema here. (Product names are checked over this whole crate by
/// `tests/neutrality.rs`, and over the schema text makinax contributes by
/// its own guard.)
const NOT_NEUTRAL: [&str; 7] = [
    "mandate",
    "package",
    "onboard",
    "owner page",
    "subscription",
    "checkout",
    "curator",
];

#[test]
fn every_tool_is_described_in_neutral_terms() {
    let mut offenders = Vec::new();
    for t in MakinaxService::router().list_all() {
        let text = format!(
            "{} {} {}",
            t.name,
            t.description.as_deref().unwrap_or_default(),
            serde_json::to_string(&t.input_schema).unwrap()
        )
        .to_lowercase();
        for w in NOT_NEUTRAL {
            if text.contains(w) {
                offenders.push(format!("{}: {w}", t.name));
            }
        }
        // "kit" only as a word, not inside another one.
        if text
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|tok| tok == "kit")
        {
            offenders.push(format!("{}: kit", t.name));
        }
        assert!(
            t.description.as_deref().is_some_and(|d| !d.is_empty()),
            "{} has no description",
            t.name
        );
    }
    assert!(offenders.is_empty(), "{offenders:?}");
}

/// In the read-only build the simulation tools say what they do:
/// eth_call at the latest block, each call on its own, no fork.
#[cfg(not(feature = "write"))]
#[test]
fn the_read_only_simulation_tools_describe_eth_call() {
    let tools = MakinaxService::router().list_all();
    let desc = |name: &str| {
        tools
            .iter()
            .find(|t| t.name == name)
            .and_then(|t| t.description.as_deref())
            .unwrap_or_default()
            .to_string()
    };
    for name in ["simulate_call", "simulate_owner_batch"] {
        let d = desc(name);
        assert!(d.contains("eth_call at the latest block"), "{name}: {d}");
        assert!(d.contains("No fork, no balance changes"), "{name}: {d}");
    }
    assert!(desc("simulate_owner_batch").contains("each call checked on its own"));
}

/// And in the read-write build they describe the fork, not eth_call.
#[cfg(feature = "write")]
#[test]
fn the_read_write_simulation_tools_describe_the_fork() {
    let tools = MakinaxService::router().list_all();
    let d = tools
        .iter()
        .find(|t| t.name == "simulate_owner_batch")
        .and_then(|t| t.description.as_deref())
        .unwrap_or_default();
    assert!(d.contains("local fork"), "{d}");
    assert!(!d.contains("eth_call"), "{d}");
}

/// The repository README and the skill name the real tools: the README lists every
/// tool, and every tool-shaped name the skill cites (`get_…`, `build_…`,
/// …) is one. Neither uses a product concept.
#[test]
fn the_readme_and_the_skill_name_only_real_tools() {
    let mut all: BTreeSet<String> = READ_CLASS.iter().map(|s| s.to_string()).collect();
    all.insert("execute_call".to_string());
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let read = |f: &str| std::fs::read_to_string(root.join(f)).unwrap();
    let readme = read("../../README.md");
    let skill = read("skills/makinax/SKILL.md");
    for tool in &all {
        assert!(readme.contains(&format!("`{tool}`")), "README lacks {tool}");
    }
    const VERBS: [&str; 8] = [
        "get_",
        "list_",
        "build_",
        "check_",
        "encode_",
        "simulate_",
        "execute_",
        "preview_",
    ];
    let cited: Vec<&str> = skill
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|t| VERBS.iter().any(|v| t.starts_with(v)))
        .collect();
    assert!(cited.len() >= 10, "the skill cites the tools: {cited:?}");
    for t in cited {
        assert!(
            all.contains(t),
            "the skill names `{t}`, which is not a tool"
        );
    }
    let front = format!(
        "---\nname: makinax\nversion: {}\n",
        env!("CARGO_PKG_VERSION")
    );
    assert!(
        skill.starts_with(&front),
        "the skill's version is the crate's"
    );
    for (name, text) in [("README.md", &readme), ("SKILL.md", &skill)] {
        let lower = text.to_lowercase();
        for w in NOT_NEUTRAL {
            assert!(!lower.contains(w), "{name}: {w}");
        }
    }
}
