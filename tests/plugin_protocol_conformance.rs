//! Conformance against the `nostdb-spec` plugin protocol fixtures.
//!
//! Thirty-two messages and handshakes, and none of them starts a process. A suite that started a
//! plugin to check starting one would be executing arbitrary code to test the rules that decide
//! whether to.

use nostdb_cli::plugin_install::{Installation, Scope};
use nostdb_cli::plugin_run::{Ready, check_handshake, check_invoke};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn fixture_root() -> Option<PathBuf> {
    let raw = std::env::var("NOSTDB_SPEC_FIXTURES").ok()?;
    let directory = PathBuf::from(raw).join("plugin-protocol");
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

fn documents(directory: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", directory.display()))
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    paths.sort();
    paths
}

fn json(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path).expect("fixture is UTF-8");
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// A `Ready` standing for an approval, so a reply can be checked against one.
fn approved(name: &str, actions: &[String], outputs: &[&str]) -> Ready {
    Ready {
        installation: Installation {
            name: name.to_owned(),
            repository: "https://github.com/e/v".to_owned(),
            commit: "a".repeat(40),
            subdirectory: None,
            manifest_digest: format!("sha256:{}", "1".repeat(64)),
            tree_digest: format!("sha256:{}", "2".repeat(64)),
            scope: Scope::Project,
            manifest_version: 1,
            plugin_version: "1.0.0".to_owned(),
            approved_permissions: serde_json::json!({
                "graph_read": true,
                "database_write": false,
                "output_paths": outputs,
                "network_hosts": [],
            }),
        },
        directory: PathBuf::from("/p/.nostdb/plugins").join(name),
        command: vec!["bin/tool".to_owned()],
        declared_actions: actions.to_vec(),
        graph_read: true,
        output_paths: outputs.iter().map(|o| (*o).to_owned()).collect(),
    }
}

#[test]
fn every_handshake_fixture_reproduces_its_declared_outcome() {
    let Some(root) = fixture_root() else {
        println!("plugin protocol conformance: skipped, NOSTDB_SPEC_FIXTURES is unset");
        return;
    };
    let paths = documents(&root.join("handshake"));
    assert!(!paths.is_empty(), "no handshake fixtures were found");

    for path in &paths {
        let fixture = json(path);
        let name = fixture["approved"]["name"].as_str().expect("a name");
        let actions: Vec<String> = fixture["approved"]["actions"]
            .as_array()
            .expect("actions")
            .iter()
            .filter_map(|action| action.as_str().map(str::to_owned))
            .collect();
        let ready = approved(name, &actions, &[]);
        let reply = fixture["handshake"].to_string();

        let expected = expectations(path);
        let declared = expected
            .get("outcome")
            .cloned()
            .unwrap_or_else(|| panic!("{} declares no outcome", path.display()));

        match check_handshake(&reply, &ready) {
            Ok(_) => assert_eq!(
                declared,
                "accept",
                "{} is rejected by the specification and accepted here",
                path.display()
            ),
            Err(error) => {
                assert_eq!(
                    declared,
                    "reject",
                    "{} is accepted by the specification and refused here: {error}",
                    path.display()
                );
                let code = expected
                    .get("code")
                    .unwrap_or_else(|| panic!("{} declares no code", path.display()));
                assert_eq!(
                    &error.code,
                    code,
                    "{} declares {code} and reports {}",
                    path.display(),
                    error.code
                );
            }
        }
    }
    println!(
        "plugin protocol conformance: {} handshakes verified",
        paths.len()
    );
}

#[test]
fn every_accepted_reply_is_read() {
    let Some(root) = fixture_root() else {
        println!("plugin protocol conformance: skipped, NOSTDB_SPEC_FIXTURES is unset");
        return;
    };
    // Only replies are read here: a request is what this build *sends*, and the fixtures for those
    // are checked against what it composes rather than against a reader it does not have.
    let mut read = 0usize;
    for path in documents(&root.join("message").join("valid")) {
        let expected = expectations(&path);
        if expected.get("role").map(String::as_str) != Some("reply") {
            continue;
        }
        let kind = expected.get("kind").cloned().unwrap_or_default();
        let message = json(&path);
        let text = message.to_string();
        // Every glob, so an output fixture is not refused for a reason the fixture is not about.
        let ready = approved("org.nostdb.view-webgpu", &["view".to_owned()], &["**"]);

        match kind.as_str() {
            "handshake" => {
                check_handshake(&text, &ready).unwrap_or_else(|error| {
                    panic!(
                        "{} is accepted by the specification: {error}",
                        path.display()
                    )
                });
            }
            "invoke" => {
                check_invoke(&text, &ready).unwrap_or_else(|error| {
                    panic!(
                        "{} is accepted by the specification: {error}",
                        path.display()
                    )
                });
            }
            // An error reply is a well-formed message that carries a refusal. Reading it must
            // surface that refusal rather than accept it as an outcome.
            "error" => {
                let error = check_invoke(&text, &ready)
                    .expect_err("an error reply is a refusal, not a result");
                let declared = expected
                    .get("code")
                    .unwrap_or_else(|| panic!("{} declares no code", path.display()));
                assert_eq!(&error.code, declared, "{}", path.display());
            }
            other => panic!("{}: unknown kind {other}", path.display()),
        }
        read += 1;
    }
    assert!(read > 0, "no reply fixtures were read");
    println!("plugin protocol conformance: {read} accepted replies verified");
}

#[test]
fn every_rejected_reply_reports_the_declared_code() {
    let Some(root) = fixture_root() else {
        println!("plugin protocol conformance: skipped, NOSTDB_SPEC_FIXTURES is unset");
        return;
    };
    let ready = approved("org.nostdb.view-webgpu", &["view".to_owned()], &["**"]);
    let mut checked = 0usize;

    for path in documents(&root.join("message").join("invalid")) {
        let expected = expectations(&path);
        // A request this build sends is not one it reads, so the invalid-request fixtures are
        // outside what this reader can be held to. The fixture's declared role decides that, not a
        // guess from its shape, so a fixture cannot quietly fall out of scope.
        if expected.get("role").map(String::as_str) != Some("reply") {
            continue;
        }
        let message = json(&path);
        let text = message.to_string();
        let declared = expected
            .get("code")
            .cloned()
            .unwrap_or_else(|| panic!("{} declares no code", path.display()));

        let error = if message["reply"].as_str() == Some("handshake") {
            check_handshake(&text, &ready).expect_err("refused")
        } else {
            check_invoke(&text, &ready).expect_err("refused")
        };
        assert_eq!(
            error.code,
            declared,
            "{} declares {declared} and reports {}: {}",
            path.display(),
            error.code,
            error.reason
        );
        checked += 1;
    }
    assert!(checked > 0, "no rejected reply fixtures were checked");
    println!("plugin protocol conformance: {checked} rejected replies verified");
}
