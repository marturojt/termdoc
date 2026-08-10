//! Pins down the dependency-graph edges from docs/DESIGN.md §3.
//!
//! The pipeline's separation of stages is not a convention that depends on whoever writes the
//! code remembering it: it lives in the `Cargo.toml` files. A reader that adds
//! `termdoc-backend` to its dependencies fails **here**, with a message explaining why that
//! edge does not exist.
//!
//! Reading the manifests instead of invoking `cargo tree` is deliberate: it is instant, it
//! does not depend on the network or the lockfile, and the failure points at the exact file
//! that needs fixing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The internal dependencies allowed per crate. Anything else is an error.
const ALLOWED: &[(&str, &[&str])] = &[
    // The shared vocabulary cannot depend on anyone: it is the root.
    ("termdoc-core", &[]),
    // Terminal capabilities are a fact about the environment, not about the document.
    ("termdoc-term", &[]),
    ("termdoc-layout", &["termdoc-core", "termdoc-term"]),
    // It consumes `Line`, which lives in core precisely so this edge does not need to
    // include termdoc-layout.
    ("termdoc-backend", &["termdoc-core", "termdoc-term"]),
    // A reader only describes the document. If it could see a backend, it would end up
    // emitting ANSI and the ability to add backends would be lost.
    ("termdoc-read-text", &["termdoc-core"]),
    // The CLI is the only crate that wires things together: it may see everything.
    (
        "termdoc-cli",
        &[
            "termdoc-core",
            "termdoc-term",
            "termdoc-layout",
            "termdoc-backend",
            "termdoc-read-text",
        ],
    ),
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("the workspace root must exist")
}

/// Extracts the `termdoc-*` names from a manifest's dependency sections.
///
/// Hand-parsed rather than using a TOML crate: the only question is "which termdoc crates are
/// mentioned as dependencies", and that does not need a syntax tree.
fn internal_deps(manifest: &str) -> BTreeSet<String> {
    let mut deps = BTreeSet::new();
    let mut in_deps_section = false;

    for line in manifest.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with('[') {
            // Regular and platform-specific dependencies count, but dev-dependencies do not:
            // a test may use whatever it needs without that meaning the crate depends on it
            // in production.
            in_deps_section = trimmed.ends_with("dependencies]")
                && !trimmed.contains("dev-dependencies")
                && !trimmed.contains("build-dependencies");
            continue;
        }

        if !in_deps_section || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // The key may appear as `termdoc-core = ...` or in the dotted form,
        // `termdoc-core.workspace = true`. A crate name never contains a dot, so taking the
        // first segment handles both.
        let name = trimmed
            .split('=')
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches('"')
            .split('.')
            .next()
            .unwrap_or("")
            .trim();
        if name.starts_with("termdoc-") {
            deps.insert(name.to_string());
        }
    }
    deps
}

#[test]
fn the_dependency_graph_respects_the_layering() {
    let root = workspace_root();

    for (crate_name, allowed) in ALLOWED {
        let manifest_path = root.join("crates").join(crate_name).join("Cargo.toml");
        let manifest = std::fs::read_to_string(&manifest_path)
            .unwrap_or_else(|e| panic!("could not read {}: {e}", manifest_path.display()));

        let actual = internal_deps(&manifest);
        let allowed: BTreeSet<String> = allowed.iter().map(|s| s.to_string()).collect();

        let forbidden: Vec<_> = actual.difference(&allowed).collect();
        assert!(
            forbidden.is_empty(),
            "\n{crate_name} depends on {forbidden:?}, which breaks the layering in \
             docs/DESIGN.md §3.\n\
             Allowed for this crate: {allowed:?}\n\
             Manifest: {}\n\n\
             If the edge is genuinely necessary, the design changes first: update \
             docs/DESIGN.md §3 and this test's ALLOWED table, and explain why.\n",
            manifest_path.display()
        );
    }
}

#[test]
fn every_workspace_crate_is_covered() {
    // A new crate with no ALLOWED entry would go unwatched, which is exactly how an
    // architecture erodes.
    let root = workspace_root();
    let covered: BTreeSet<&str> = ALLOWED.iter().map(|(n, _)| *n).collect();

    let mut found = BTreeSet::new();
    for entry in std::fs::read_dir(root.join("crates")).expect("crates/ must exist") {
        let entry = entry.expect("a readable directory entry");
        if !entry.path().join("Cargo.toml").exists() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        found.insert(name);
    }

    for name in &found {
        assert!(
            covered.contains(name.as_str()),
            "the crate '{name}' is missing from the ALLOWED table in tests/layering.rs; \
             add it with its permitted dependencies"
        );
    }
}

#[test]
fn core_is_the_root_of_the_graph() {
    // Checked independently because it is the invariant everything else hangs off: if core
    // depended on anything, it could no longer be the shared vocabulary.
    let root = workspace_root();
    let manifest = std::fs::read_to_string(root.join("crates/termdoc-core/Cargo.toml")).unwrap();
    assert!(
        internal_deps(&manifest).is_empty(),
        "termdoc-core cannot depend on any termdoc crate"
    );
}

#[cfg(test)]
mod parser {
    use super::internal_deps;

    #[test]
    fn it_ignores_dev_dependencies() {
        let manifest = "\
[dependencies]
termdoc-core = { path = \"x\" }

[dev-dependencies]
termdoc-backend = { path = \"y\" }
";
        let deps = internal_deps(manifest);
        assert!(deps.contains("termdoc-core"));
        assert!(
            !deps.contains("termdoc-backend"),
            "a test dependency is not a production edge"
        );
    }

    #[test]
    fn it_counts_platform_specific_dependencies() {
        let manifest = "\
[target.'cfg(unix)'.dependencies]
termdoc-core = { path = \"x\" }
";
        assert!(internal_deps(manifest).contains("termdoc-core"));
    }

    #[test]
    fn it_ignores_comments_and_external_crates() {
        let manifest = "\
[dependencies]
# termdoc-backend = { path = \"no\" }
clap = \"4\"
termdoc-core.workspace = true
";
        let deps = internal_deps(manifest);
        assert_eq!(deps.len(), 1);
        assert!(deps.contains("termdoc-core"));
    }
}
