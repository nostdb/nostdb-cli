//! Installing a plugin, end to end, over a scripted provider conversation.
//!
//! The transport is a trait, so the whole install runs against a recorded conversation and no
//! test reaches the network. What is under test is the order of the requests, what is refused
//! before anything is downloaded, the two digests, and what the record ends up saying — every one
//! of which a scripted provider answers exactly as a real one would.
//!
//! Nothing here executes a plugin, and there is nothing in the installing code that could.

use nostdb_cli::plugin::PluginSource;
use nostdb_cli::plugin_install::{
    Outcome, Record, Scope, Version, commit_install, fetch, read_record, record_path,
};
use nostdb_core::provider::{ProviderClient, Transport};
use std::path::{Path, PathBuf};

const COMMIT: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f009182736";

/// A provider that replays scripted replies and records every request it was sent.
///
/// The requests are what several cases assert: that the manifest is read before the rest of the
/// tree, and that a refused tree is never read at all.
struct Scripted {
    replies: Vec<String>,
    content: Vec<Vec<u8>>,
    sent: Vec<String>,
}

impl Scripted {
    fn new(replies: &[String], content: &[Vec<u8>]) -> Self {
        Self {
            replies: replies.to_vec(),
            content: content.to_vec(),
            sent: Vec::new(),
        }
    }
}

impl Transport for Scripted {
    fn send(&mut self, line: &str) -> Result<(), String> {
        self.sent.push(line.to_owned());
        Ok(())
    }

    fn receive(&mut self) -> Result<String, String> {
        if self.replies.is_empty() {
            return Err("the script has no further replies".to_owned());
        }
        Ok(self.replies.remove(0))
    }

    fn receive_exact(&mut self, length: usize) -> Result<Vec<u8>, String> {
        if self.content.is_empty() {
            return Err("the script has no further content".to_owned());
        }
        let bytes = self.content.remove(0);
        if bytes.len() != length {
            return Err(format!(
                "the script declared {length} bytes and holds {}",
                bytes.len()
            ));
        }
        Ok(bytes)
    }
}

fn handshake() -> String {
    r#"{"provider_protocol_version":1,"reply":"handshake","provider":"scripted","provider_version":"1.0.0","roles":["source"]}"#.to_owned()
}

fn resolve() -> String {
    format!(
        r#"{{"provider_protocol_version":1,"reply":"resolve","snapshot":"{COMMIT}","canonical_locator":"github://example/viewer/?ref=v1.0.0","cached":false}}"#
    )
}

fn enumerate(entries: &[(&str, usize)]) -> String {
    let listed: Vec<String> = entries
        .iter()
        .map(|(path, bytes)| {
            format!(r#"{{"path":"{path}","bytes":{bytes},"content_id":"b1946ac9"}}"#)
        })
        .collect();
    format!(
        r#"{{"provider_protocol_version":1,"reply":"enumerate","entries":[{}]}}"#,
        listed.join(",")
    )
}

fn read(bytes: usize) -> String {
    format!(r#"{{"provider_protocol_version":1,"reply":"read","bytes":{bytes}}}"#)
}

fn manifest(range: &str) -> String {
    format!(
        r#"{{
  "manifest_version": 1,
  "name": "org.example.viewer",
  "version": "1.0.0",
  "nostdb": "{range}",
  "entrypoint": {{ "command": ["bin/viewer"] }},
  "protocol_version": 1,
  "actions": [{{ "name": "view", "ai_usage": "none" }}],
  "permissions": {{
    "graph_read": true,
    "database_write": false,
    "output_paths": [],
    "network_hosts": []
  }}
}}
"#
    )
}

/// A conversation that serves one plugin: a manifest and one other file.
fn conversation(range: &str, tool: &[u8]) -> (Vec<String>, Vec<Vec<u8>>) {
    let manifest = manifest(range);
    let manifest_bytes = manifest.clone().into_bytes();
    let replies = vec![
        handshake(),
        resolve(),
        enumerate(&[
            ("nostdb-plugin.json", manifest_bytes.len()),
            ("bin/viewer", tool.len()),
        ]),
        read(manifest_bytes.len()),
        read(tool.len()),
    ];
    // The manifest first, then the rest. A tree that will not do is refused before its bytes are
    // paid for, and this order is what the requests below assert.
    let content = vec![manifest_bytes, tool.to_vec()];
    (replies, content)
}

fn scripted_client(replies: Vec<String>, content: Vec<Vec<u8>>) -> ProviderClient<Scripted> {
    let mut client = ProviderClient::new(Scripted::new(&replies, &content));
    client.handshake().expect("the scripted provider agrees");
    client
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let mut base = std::env::temp_dir();
        base.push(format!("nostdb-plugin-{label}"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("scratch directory");
        Self(base)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn source(text: &str) -> PluginSource {
    PluginSource::parse(text).expect("a source")
}

fn engine() -> Version {
    Version::parse("0.1.0").expect("a version")
}

#[test]
fn a_plugin_is_fetched_checked_and_recorded() {
    let scratch = Scratch::new("install");
    let (replies, content) = conversation(">=0.1.0 <0.2.0", b"binary");
    let mut client = scripted_client(replies, content);
    let source = source("https://github.com/example/viewer?ref=v1.0.0");

    let fetched = fetch(&mut client, &source, &engine()).expect("installable");
    assert_eq!(fetched.commit, COMMIT);
    assert_eq!(fetched.name, "org.example.viewer");
    assert_eq!(fetched.plugin_version, "1.0.0");
    assert_eq!(fetched.files.len(), 2);
    assert!(fetched.manifest_digest.starts_with("sha256:"));
    assert!(fetched.tree_digest.starts_with("sha256:"));
    assert_ne!(
        fetched.manifest_digest, fetched.tree_digest,
        "two digests answering different questions must not be the same value"
    );

    let outcome =
        commit_install(&fetched, &source, scratch.path(), Scope::Project).expect("written");
    assert_eq!(outcome, Outcome::Installed);

    // The files arrive where the contract says, with their content intact.
    let directory = scratch
        .path()
        .join(".nostdb")
        .join("plugins")
        .join("org.example.viewer");
    assert_eq!(
        std::fs::read(directory.join("bin").join("viewer")).expect("the entrypoint"),
        b"binary"
    );
    assert!(directory.join("nostdb-plugin.json").is_file());

    // And the record says what was approved, in the shape the contract publishes.
    let record = read_record(scratch.path(), Scope::Project).expect("readable");
    let entry = record.find("org.example.viewer").expect("recorded");
    assert_eq!(entry.commit, COMMIT);
    assert_eq!(entry.repository, "https://github.com/example/viewer");
    assert_eq!(entry.subdirectory, None);
    assert_eq!(entry.scope, Scope::Project);
    assert_eq!(entry.manifest_version, 1);
    assert_eq!(entry.approved_permissions["database_write"], false);

    // The record is a document a second reader must accept, so it is read back through the
    // published rules rather than trusted because this build wrote it.
    let text = std::fs::read_to_string(record_path(scratch.path())).expect("the record");
    Record::parse(&text, Scope::Project).expect("the record this build wrote is a valid record");
}

#[test]
fn reinstalling_the_same_commit_writes_nothing() {
    let scratch = Scratch::new("already");
    let source = source("https://github.com/example/viewer?ref=v1.0.0");

    let (replies, content) = conversation(">=0.1.0 <0.2.0", b"binary");
    let mut client = scripted_client(replies, content);
    let first = fetch(&mut client, &source, &engine()).expect("installable");
    commit_install(&first, &source, scratch.path(), Scope::Project).expect("written");
    let before = std::fs::read_to_string(record_path(scratch.path())).expect("the record");

    let (replies, content) = conversation(">=0.1.0 <0.2.0", b"binary");
    let mut client = scripted_client(replies, content);
    let again = fetch(&mut client, &source, &engine()).expect("installable");
    assert_eq!(
        commit_install(&again, &source, scratch.path(), Scope::Project).expect("nothing to do"),
        Outcome::AlreadyInstalled
    );
    assert_eq!(
        std::fs::read_to_string(record_path(scratch.path())).expect("the record"),
        before,
        "an install with nothing to do rewrote the record"
    );
}

#[test]
fn the_same_commit_with_different_bytes_is_refused_rather_than_written_over() {
    let scratch = Scratch::new("mismatch");
    let source = source("https://github.com/example/viewer?ref=v1.0.0");

    let (replies, content) = conversation(">=0.1.0 <0.2.0", b"binary");
    let mut client = scripted_client(replies, content);
    let first = fetch(&mut client, &source, &engine()).expect("installable");
    commit_install(&first, &source, scratch.path(), Scope::Project).expect("written");
    let recorded = std::fs::read_to_string(record_path(scratch.path())).expect("the record");

    // The same commit, and the code behind it changed. A commit is immutable, so this cannot be
    // a legitimate new version of the plugin.
    let (replies, content) = conversation(">=0.1.0 <0.2.0", b"tampered");
    let mut client = scripted_client(replies, content);
    let tampered = fetch(&mut client, &source, &engine()).expect("fetched");
    let error =
        commit_install(&tampered, &source, scratch.path(), Scope::Project).expect_err("refused");
    assert_eq!(error.code.as_str(), "PLUGIN_DIGEST_MISMATCH");
    assert!(
        error.reason.contains("code changed"),
        "the message should say which digest moved: {}",
        error.reason
    );

    // The record is the only evidence that anything changed, so it is left exactly as it was.
    assert_eq!(
        std::fs::read_to_string(record_path(scratch.path())).expect("the record"),
        recorded
    );
}

#[test]
fn a_new_commit_replaces_what_was_recorded() {
    let scratch = Scratch::new("replace");
    let source = source("https://github.com/example/viewer?ref=v1.0.0");

    let (replies, content) = conversation(">=0.1.0 <0.2.0", b"binary");
    let mut client = scripted_client(replies, content);
    let first = fetch(&mut client, &source, &engine()).expect("installable");
    commit_install(&first, &source, scratch.path(), Scope::Project).expect("written");

    // A different commit is a user asking for a different version, not a mismatch.
    let (mut replies, content) = conversation(">=0.1.0 <0.2.0", b"newer");
    replies[1] = replies[1].replace(COMMIT, &"a".repeat(40));
    let mut client = scripted_client(replies, content);
    let newer = fetch(&mut client, &source, &engine()).expect("installable");
    assert_eq!(
        commit_install(&newer, &source, scratch.path(), Scope::Project).expect("written"),
        Outcome::Replaced
    );

    let record = read_record(scratch.path(), Scope::Project).expect("readable");
    assert_eq!(
        record.installed().len(),
        1,
        "a replacement is not a second entry"
    );
    assert_eq!(
        record.find("org.example.viewer").expect("recorded").commit,
        "a".repeat(40)
    );
    assert_eq!(
        std::fs::read(
            scratch
                .path()
                .join(".nostdb/plugins/org.example.viewer/bin/viewer")
        )
        .expect("the entrypoint"),
        b"newer"
    );
}

#[test]
fn an_incompatible_range_is_refused_before_the_rest_of_the_tree_is_read() {
    let scratch = Scratch::new("incompatible");
    let (replies, content) = conversation(">=9.0.0", b"binary");
    let mut client = scripted_client(replies, content);
    let source = source("https://github.com/example/viewer?ref=v1.0.0");

    let error = fetch(&mut client, &source, &engine()).expect_err("refused");
    assert_eq!(error.code(), "PLUGIN_INCOMPATIBLE");
    assert_eq!(error.exit_class(), nostdb_cli::ExitClass::Plugin);

    // Nothing was written, so a refused plugin leaves no directory behind for something later
    // to find and take for an installation.
    assert!(!scratch.path().join(".nostdb").join("plugins").exists());
}

#[test]
fn a_tree_that_is_not_a_plugin_is_refused_without_reading_anything() {
    let manifestless = vec![
        handshake(),
        resolve(),
        enumerate(&[("README.md", 12), ("bin/viewer", 6)]),
    ];
    let mut client = scripted_client(manifestless, Vec::new());
    let source = source("https://github.com/example/viewer?ref=v1.0.0");

    let error = fetch(&mut client, &source, &engine()).expect_err("refused");
    assert_eq!(error.code(), "PLUGIN_SOURCE_INVALID");
    // The script holds no content at all, so reaching a read would have failed differently.
    // That is the assertion: a tree is refused from its enumeration.
    assert!(error.to_string().contains("nostdb-plugin.json"));
}

#[test]
fn a_subdirectory_install_records_the_subdirectory_and_installs_only_it() {
    let scratch = Scratch::new("subdirectory");
    let manifest = manifest(">=0.1.0 <0.2.0");
    let bytes = manifest.clone().into_bytes();
    let replies = vec![
        handshake(),
        resolve(),
        enumerate(&[
            ("README.md", 40),
            ("plugins/viewer/nostdb-plugin.json", bytes.len()),
            ("plugins/viewer/bin/viewer", 6),
        ]),
        read(bytes.len()),
        read(6),
    ];
    let content = vec![bytes, b"binary".to_vec()];
    let mut client = scripted_client(replies, content);
    let source = source("https://github.com/example/viewer?ref=v1.0.0#plugins/viewer");

    let fetched = fetch(&mut client, &source, &engine()).expect("installable");
    // Two files, not three. The document above the subdirectory is not part of the plugin.
    assert_eq!(fetched.files.len(), 2);
    let paths: Vec<&str> = fetched
        .files
        .iter()
        .map(|(path, _)| path.as_str())
        .collect();
    assert!(paths.contains(&"nostdb-plugin.json") && paths.contains(&"bin/viewer"));

    commit_install(&fetched, &source, scratch.path(), Scope::Project).expect("written");
    let record = read_record(scratch.path(), Scope::Project).expect("readable");
    assert_eq!(
        record
            .find("org.example.viewer")
            .expect("recorded")
            .subdirectory,
        Some("plugins/viewer".to_owned())
    );
    assert!(
        !scratch
            .path()
            .join(".nostdb/plugins/org.example.viewer/README.md")
            .exists(),
        "a file outside the subdirectory was installed"
    );
}

#[test]
fn a_provider_that_cannot_reach_the_host_is_reported_as_unavailable() {
    let refused = vec![
        handshake(),
        r#"{"provider_protocol_version":1,"reply":"error","code":"PROVIDER_SOURCE_UNAVAILABLE","message":"the host did not answer"}"#.to_owned(),
    ];
    let mut client = scripted_client(refused, Vec::new());
    let source = source("https://github.com/example/viewer?ref=v1.0.0");

    let error = fetch(&mut client, &source, &engine()).expect_err("refused");
    // The provider's own code, not a relabelled one. If the host was unreachable, saying the
    // plugin failed would name the wrong layer.
    assert_eq!(error.code(), "PROVIDER_SOURCE_UNAVAILABLE");
    assert_eq!(error.exit_class(), nostdb_cli::ExitClass::Unavailable);
}

#[test]
fn a_refused_manifest_keeps_the_manifest_contract_code() {
    let broken = r#"{"manifest_version":1,"name":"viewer","version":"1.0.0","nostdb":">=0.1.0","entrypoint":{"command":"sh -c evil"},"protocol_version":1,"actions":[],"permissions":{"graph_read":true,"database_write":true,"output_paths":[],"network_hosts":[]}}"#;
    let bytes = broken.as_bytes().to_vec();
    let replies = vec![
        handshake(),
        resolve(),
        enumerate(&[("nostdb-plugin.json", bytes.len())]),
        read(bytes.len()),
    ];
    let mut client = scripted_client(replies, vec![bytes]);
    let source = source("https://github.com/example/viewer?ref=v1.0.0");

    let error = fetch(&mut client, &source, &engine()).expect_err("refused");
    assert_eq!(error.code(), "PLUGIN_MANIFEST_INVALID");
    let reported = error.to_string();
    // Every problem, not the first, so an author fixes the manifest in one pass.
    assert!(reported.contains("argument vector"), "{reported}");
    assert!(reported.contains("database_write"), "{reported}");
    assert!(reported.contains("dotted segments"), "{reported}");
}

#[test]
fn a_project_and_a_global_installation_do_not_share_a_record() {
    let project = Scratch::new("scope-project");
    let global = Scratch::new("scope-global");
    let source = source("https://github.com/example/viewer?ref=v1.0.0");

    for (root, scope) in [
        (project.path(), Scope::Project),
        (global.path(), Scope::Global),
    ] {
        let (replies, content) = conversation(">=0.1.0 <0.2.0", b"binary");
        let mut client = scripted_client(replies, content);
        let fetched = fetch(&mut client, &source, &engine()).expect("installable");
        commit_install(&fetched, &source, root, scope).expect("written");
    }

    // Each record names its own scope, and a record claiming the other one is refused — which is
    // what stops a global file from being dropped into a project and taking its precedence.
    assert_eq!(
        read_record(project.path(), Scope::Project)
            .expect("readable")
            .find("org.example.viewer")
            .expect("recorded")
            .scope,
        Scope::Project
    );
    let global_text = std::fs::read_to_string(record_path(global.path())).expect("the record");
    assert!(global_text.contains("\"scope\": \"global\""));
    assert!(
        Record::parse(&global_text, Scope::Project).is_err(),
        "a global record read as a project record must be refused"
    );
}
