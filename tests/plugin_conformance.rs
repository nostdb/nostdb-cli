//! Conformance against the `nostdb-spec` plugin fixtures.
//!
//! Twenty-two manifests and sources, and none of them installs anything. Here that is not
//! only the convenience it is for the other suites: a test that installed a plugin to check
//! installation would be executing the thing the contract exists to keep from executing.

use nostdb_cli::plugin::{PluginSource, validate_manifest};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn fixture_root() -> Option<PathBuf> {
    let raw = std::env::var("NOSTDB_SPEC_FIXTURES").ok()?;
    let directory = PathBuf::from(raw).join("plugin");
    directory.is_dir().then_some(directory)
}

fn expectations(path: &Path) -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(path.with_extension("expected")).unwrap_or_else(|error| {
        panic!(
            "cannot read the expectation for {}: {error}",
            path.display()
        )
    });
    text.lines()
        .filter_map(|line| line.split_once(" = "))
        .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
        .collect()
}

fn documents(directory: &Path, extension: &str) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", directory.display()))
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.extension().and_then(|s| s.to_str()) == Some(extension))
        .collect();
    paths.sort();
    paths
}

#[test]
fn every_accepted_manifest_is_read() {
    let Some(root) = fixture_root() else {
        println!("plugin conformance: skipped, NOSTDB_SPEC_FIXTURES is unset");
        return;
    };
    let paths = documents(&root.join("manifest").join("valid"), "json");
    assert!(!paths.is_empty(), "no accepted manifests were found");
    for path in &paths {
        let text = std::fs::read_to_string(path).expect("fixture is UTF-8");
        if let Err((code, problems)) = validate_manifest(&text) {
            panic!(
                "{} is accepted by the specification and refused here as {code}: {problems:?}",
                path.display()
            );
        }
    }
    println!(
        "plugin conformance: {} accepted manifests verified",
        paths.len()
    );
}

#[test]
fn every_rejected_manifest_is_refused_with_the_declared_code() {
    // The code matters as much as the refusal. Reporting an invalid manifest where the
    // contract says the version is unreadable would send an author looking for a malformed
    // field that is not there.
    let Some(root) = fixture_root() else {
        println!("plugin conformance: skipped, NOSTDB_SPEC_FIXTURES is unset");
        return;
    };
    let paths = documents(&root.join("manifest").join("invalid"), "json");
    assert!(!paths.is_empty(), "no rejected manifests were found");
    for path in &paths {
        let expected = expectations(path);
        let declared = expected
            .get("code")
            .unwrap_or_else(|| panic!("{} declares no code", path.display()));
        let text = std::fs::read_to_string(path).expect("fixture is UTF-8");
        let Err((code, problems)) = validate_manifest(&text) else {
            panic!(
                "{} is rejected by the specification and accepted here",
                path.display()
            );
        };
        assert_eq!(
            code.as_str(),
            declared,
            "{} was refused with the wrong code: {problems:?}",
            path.display()
        );
        assert!(!problems.is_empty(), "a refusal must say what was wrong");
    }
    println!(
        "plugin conformance: {} rejected manifests verified",
        paths.len()
    );
}

#[test]
fn every_source_matches_its_declared_outcome() {
    let Some(root) = fixture_root() else {
        println!("plugin conformance: skipped, NOSTDB_SPEC_FIXTURES is unset");
        return;
    };
    let mut count = 0_usize;
    for (kind, accepted) in [("valid", true), ("invalid", false)] {
        let paths = documents(&root.join("source").join(kind), "txt");
        assert!(!paths.is_empty(), "no {kind} sources were found");
        for path in &paths {
            let expected = expectations(path);
            let text = std::fs::read_to_string(path).expect("fixture is UTF-8");
            let parsed = PluginSource::parse(text.trim());
            assert_eq!(
                parsed.is_ok(),
                accepted,
                "{} disagrees with the specification: {parsed:?}",
                path.display()
            );
            if let (Ok(source), Some(canonical)) = (&parsed, expected.get("canonical")) {
                assert_eq!(
                    &source.to_string(),
                    canonical,
                    "{} normalized to the wrong form",
                    path.display()
                );
            }
            count += 1;
        }
    }
    println!("plugin conformance: {count} sources verified");
}

#[test]
fn a_manifest_is_refused_for_every_problem_it_has_at_once() {
    // An author fixing a manifest should need one pass, not one failed install per mistake.
    let text = r#"{
      "manifest_version": 1,
      "name": "viewer",
      "version": "1.0.0",
      "nostdb": ">=0.1.0",
      "entrypoint": {"command": "/bin/sh -c echo"},
      "protocol_version": 1,
      "actions": [{"ai_usage": "maybe"}],
      "permissions": {"database_write": true, "output_paths": ["../x"], "network_hosts": ["*"]}
    }"#;
    let (_, problems) = validate_manifest(text).expect_err("this manifest is wrong many ways");
    assert!(problems.len() >= 5, "{problems:?}");
}
