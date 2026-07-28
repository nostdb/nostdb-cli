//! `nostdb --version --json` reports every contract, checked against the registry that declares them.
//!
//! This is the check the report did not have. It was a hand-written list, and it fell six contracts
//! behind without anything noticing — while `docs/PRD.md` section 25.3 makes it the surface every
//! install route is verified at, and the Skill's Engine resolution reads it to decide whether a
//! build is compatible at all.
//!
//! The registry is `nostdb-spec/versions.json`, resolved as a sibling of the fixtures the
//! superproject passes in. Read from there rather than copied, because a copy is what this was.

use nostdb_cli::{ExitClass, Format};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The registry, when the superproject told us where the fixtures are.
fn registry() -> Option<serde_json::Value> {
    let raw = std::env::var("NOSTDB_SPEC_FIXTURES").ok()?;
    // The fixtures live beside the registry, so one path locates both. The suite runner has no
    // second variable to pass and this needs no new one.
    let path = PathBuf::from(raw).parent()?.join("versions.json");
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn report() -> serde_json::Value {
    let mut out = Vec::new();
    let class = nostdb_cli::command::version(true, &mut out);
    assert_eq!(class, ExitClass::Success);
    let text = String::from_utf8(out).expect("the report is UTF-8");
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("the report is not JSON: {error}\n{text}"))
}

/// The key the report uses for a contract key.
///
/// `nost_language_version` becomes `nost_language_versions`, because the report states what a build
/// supports and that is a list. Section 25.4 already spells it that way.
fn reported_key(contract: &str) -> String {
    format!("{}s", contract)
}

#[test]
fn the_report_is_a_json_object_naming_the_product_and_this_build() {
    let report = report();
    assert_eq!(report["product"], "nostdb");
    assert_eq!(report["engine_version"], nostdb_cli::VERSION);
}

#[test]
fn every_specified_contract_is_reported() {
    let Some(registry) = registry() else {
        println!("version conformance: skipped, NOSTDB_SPEC_FIXTURES is unset");
        return;
    };
    let report = report();
    let mut checked = 0usize;

    for contract in registry["contracts"].as_array().expect("contracts array") {
        let key = contract["key"].as_str().expect("a key");
        let status = contract["status"].as_str().expect("a status");
        let reported = reported_key(key);

        if status != "specified" {
            // A deferred contract has a reserved key and no authored contract, so nothing
            // implements it. A report claiming support for one would be false in the one place a
            // caller trusts to be exact.
            assert!(
                report.get(&reported).is_none(),
                "{key} is {status} and the report claims support for it"
            );
            continue;
        }

        let versions = report
            .get(&reported)
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| {
                panic!("the report states no {reported}, which the registry declares as specified")
            });
        assert!(!versions.is_empty(), "{reported} is empty");

        // The versions the registry declares supported are the versions the build must report. A
        // build reporting a version the registry does not carry is claiming something unpublished;
        // one omitting a version the registry carries is understating what it can read.
        let declared: Vec<u64> = contract["supported"]
            .as_array()
            .expect("supported array")
            .iter()
            .map(|value| value.as_u64().expect("a number"))
            .collect();
        let found: Vec<u64> = versions
            .iter()
            .map(|value| value.as_u64().expect("a number"))
            .collect();
        assert_eq!(found, declared, "{reported}");
        checked += 1;
    }

    assert!(checked > 0, "no contract was checked");
    println!("version conformance: {checked} contracts verified");
}

#[test]
fn the_report_states_nothing_the_registry_does_not_declare() {
    let Some(registry) = registry() else {
        println!("version conformance: skipped, NOSTDB_SPEC_FIXTURES is unset");
        return;
    };
    let known: BTreeMap<String, ()> = registry["contracts"]
        .as_array()
        .expect("contracts array")
        .iter()
        .map(|contract| (reported_key(contract["key"].as_str().expect("a key")), ()))
        .collect();

    // The other direction, which is the one that catches an invented key. A report naming a
    // contract nothing published would be a version a caller could ask about and nobody owns.
    for key in report().as_object().expect("an object").keys() {
        if key == "product" || key == "engine_version" {
            continue;
        }
        assert!(
            known.contains_key(key),
            "the report states {key}, which the registry does not declare"
        );
    }
    println!("version conformance: the report invents nothing");
}

#[test]
fn the_human_report_names_the_same_contracts() {
    // Two renderings of one fact. A person reading the column and a script reading the JSON should
    // not be told different things about what this build supports.
    let mut out = Vec::new();
    assert_eq!(
        nostdb_cli::command::version(false, &mut out),
        ExitClass::Success
    );
    let text = String::from_utf8(out).expect("UTF-8");

    for key in report().as_object().expect("an object").keys() {
        if key == "product" || key == "engine_version" {
            continue;
        }
        // The human column drops the `_versions` suffix, which is the style the report already
        // used for the three contracts it did report. Stripping only the trailing `s` would have
        // looked for `manifest_version` in a column that says `manifest`.
        let name = key.strip_suffix("_versions").expect("a plural key");
        assert!(
            text.contains(name),
            "the human report omits {name}:\n{text}"
        );
    }
}

#[test]
fn the_report_carries_no_commentary_on_stdout() {
    // Section 20 keeps data on stdout and diagnostics on stderr, and this is the one command whose
    // whole output a script parses. A warning printed here would make the JSON unparseable.
    let mut out = Vec::new();
    assert_eq!(
        nostdb_cli::command::version(true, &mut out),
        ExitClass::Success
    );
    let text = String::from_utf8(out).expect("UTF-8");
    assert!(text.starts_with('{'), "{text}");
    assert!(text.trim_end().ends_with('}'), "{text}");
    // And it round-trips, which is what a caller actually needs.
    let _: serde_json::Value = serde_json::from_str(&text).expect("parses");
    let _ = Format::default();
}
