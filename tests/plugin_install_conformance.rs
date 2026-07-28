//! Conformance against the `nostdb-spec` plugin installation fixtures.
//!
//! Forty-eight records, ranges, and trees, and none of them installs anything. That is not only the
//! convenience it is for the other suites: a test that installed a plugin to check installation
//! would be executing the thing the contract exists to keep from executing.
//!
//! Every rule these fixtures state is decidable from a document, a version range, or an
//! enumeration — which is the property that lets a tree be refused before a byte is downloaded.

use nostdb_cli::plugin_install::{EngineRange, Record, Scope, Version, plan};
use nostdb_core::provider::Entry;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn fixture_root() -> Option<PathBuf> {
    let raw = std::env::var("NOSTDB_SPEC_FIXTURES").ok()?;
    let directory = PathBuf::from(raw).join("plugin-install");
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

#[test]
fn every_accepted_record_is_read() {
    let Some(root) = fixture_root() else {
        println!("plugin install conformance: skipped, NOSTDB_SPEC_FIXTURES is unset");
        return;
    };
    let paths = documents(&root.join("record").join("valid"));
    assert!(!paths.is_empty(), "no accepted records were found");
    for path in &paths {
        let text = std::fs::read_to_string(path).expect("fixture is UTF-8");
        // A record's file decides its scope, and the fixture declares which file it stands for.
        let expected = expectations(path);
        let scope = expected
            .get("scope")
            .map_or(Scope::Project, |text| Scope::parse(text).expect("a scope"));
        if let Err((code, problems)) = Record::parse(&text, scope) {
            panic!(
                "{} is accepted by the specification and refused here as {code}: {problems:?}",
                path.display()
            );
        }
    }
    println!(
        "plugin install conformance: {} accepted records verified",
        paths.len()
    );
}

#[test]
fn every_rejected_record_reports_the_declared_code() {
    let Some(root) = fixture_root() else {
        println!("plugin install conformance: skipped, NOSTDB_SPEC_FIXTURES is unset");
        return;
    };
    let paths = documents(&root.join("record").join("invalid"));
    assert!(!paths.is_empty(), "no rejected records were found");
    for path in &paths {
        let text = std::fs::read_to_string(path).expect("fixture is UTF-8");
        let declared = expectations(path)
            .get("code")
            .cloned()
            .unwrap_or_else(|| panic!("{} declares no code", path.display()));

        match Record::parse(&text, Scope::Project) {
            Ok(_) => panic!(
                "{} is rejected by the specification and read here",
                path.display()
            ),
            Err((code, _)) => assert_eq!(
                code.as_str(),
                declared,
                "{} declares {declared} and reports {code}",
                path.display()
            ),
        }
    }
    println!(
        "plugin install conformance: {} rejected records verified",
        paths.len()
    );
}

#[test]
fn every_range_admits_or_excludes_the_declared_engine() {
    let Some(root) = fixture_root() else {
        println!("plugin install conformance: skipped, NOSTDB_SPEC_FIXTURES is unset");
        return;
    };
    let paths = documents(&root.join("range"));
    assert!(!paths.is_empty(), "no ranges were found");
    for path in &paths {
        let fixture = json(path);
        let text = fixture["range"].as_str().expect("range is a string");
        let engine = fixture["engine"].as_str().expect("engine is a string");
        let declared = expectations(path)
            .get("outcome")
            .cloned()
            .unwrap_or_else(|| panic!("{} declares no outcome", path.display()));

        let range = EngineRange::parse(text).unwrap_or_else(|reason| {
            panic!(
                "{} parses in the specification and is refused here: {reason}",
                path.display()
            )
        });
        let version = Version::parse(engine).unwrap_or_else(|reason| {
            panic!(
                "{}: engine {engine} does not parse: {reason}",
                path.display()
            )
        });

        let admitted = range.admits(&version);
        let expected = declared == "admit";
        assert_eq!(
            admitted,
            expected,
            "{}: `{text}` against {engine} declares {declared}",
            path.display()
        );
    }
    println!(
        "plugin install conformance: {} ranges verified",
        paths.len()
    );
}

#[test]
fn every_unparseable_range_is_refused() {
    let Some(root) = fixture_root() else {
        println!("plugin install conformance: skipped, NOSTDB_SPEC_FIXTURES is unset");
        return;
    };
    let paths = documents(&root.join("range-invalid"));
    assert!(!paths.is_empty(), "no unparseable ranges were found");
    for path in &paths {
        let text = json(path)["range"]
            .as_str()
            .expect("range is a string")
            .to_owned();
        assert!(
            EngineRange::parse(&text).is_err(),
            "{}: `{text}` is refused by the specification and parses here",
            path.display()
        );
    }
    println!(
        "plugin install conformance: {} unparseable ranges verified",
        paths.len()
    );
}

/// Expands a tree fixture into the entry list a provider would have enumerated.
///
/// `repeat` is a fixture encoding rather than a contract concept: without it the entry-count
/// fixture would be four thousand lines and nobody would read it.
fn entries(fixture: &serde_json::Value) -> Vec<Entry> {
    let mut expanded = Vec::new();
    for entry in fixture["entries"].as_array().expect("entries array") {
        let path = entry["path"].as_str().expect("path is a string");
        let bytes = entry["bytes"].as_u64().expect("bytes is a number");
        match entry["repeat"].as_u64() {
            None => expanded.push(Entry {
                path: path.to_owned(),
                bytes,
                content_id: String::new(),
            }),
            Some(count) => {
                for index in 0..count {
                    expanded.push(Entry {
                        path: format!("{path}{index}"),
                        bytes,
                        content_id: String::new(),
                    });
                }
            }
        }
    }
    expanded
}

#[test]
fn every_tree_reproduces_its_declared_outcome() {
    let Some(root) = fixture_root() else {
        println!("plugin install conformance: skipped, NOSTDB_SPEC_FIXTURES is unset");
        return;
    };
    let paths = documents(&root.join("tree"));
    assert!(!paths.is_empty(), "no trees were found");

    let mut accepted = 0usize;
    let mut rejected = 0usize;
    for path in &paths {
        let fixture = json(path);
        let listing = entries(&fixture);
        let subdirectory = fixture["subdirectory"].as_str();
        let expected = expectations(path);
        let outcome = expected
            .get("outcome")
            .cloned()
            .unwrap_or_else(|| panic!("{} declares no outcome", path.display()));

        match plan(&listing, subdirectory) {
            Ok(planned) => {
                assert_eq!(
                    outcome,
                    "accept",
                    "{} is rejected by the specification and accepted here",
                    path.display()
                );
                // The count matters as much as the acceptance. A build that ignored the
                // subdirectory would accept this tree too, and install the wrong files.
                let declared: usize = expected
                    .get("accepted_entries")
                    .unwrap_or_else(|| panic!("{} declares no accepted_entries", path.display()))
                    .parse()
                    .expect("accepted_entries is a number");
                assert_eq!(
                    planned.len(),
                    declared,
                    "{} declares {declared} accepted entries",
                    path.display()
                );
                accepted += 1;
            }
            Err(error) => {
                assert_eq!(
                    outcome,
                    "reject",
                    "{} is accepted by the specification and refused here: {error}",
                    path.display()
                );
                let declared = expected
                    .get("code")
                    .unwrap_or_else(|| panic!("{} declares no code", path.display()));
                assert_eq!(
                    error.code.as_str(),
                    declared,
                    "{} declares {declared} and reports {}",
                    path.display(),
                    error.code
                );
                rejected += 1;
            }
        }
    }
    println!(
        "plugin install conformance: {accepted} accepted and {rejected} rejected trees verified"
    );
}
