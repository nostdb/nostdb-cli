//! Running a plugin, end to end, against a real installed directory and a scripted conversation.
//!
//! The directory and the record are real, so the digest re-check is exercised against bytes on disk
//! rather than against a value handed to it. The conversation is scripted, so no test starts a
//! process — which is also the only way to test a plugin that lies, since a well-behaved one cannot
//! be asked to.

use nostdb_cli::plugin_install::{Installation, Record, Scope, plugins_directory, write_record};
use nostdb_cli::plugin_run::{ProtocolCode, Ready, converse, preflight, recompute_digests};
use nostdb_core::provider::Transport;
use nostdb_core::sync::digest_bytes;
use std::path::{Path, PathBuf};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let mut base = std::env::temp_dir();
        base.push(format!("nostdb-run-{label}"));
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

/// A conversation the plugin side is playing from a script.
struct Scripted {
    replies: Vec<String>,
    sent: Vec<String>,
}

impl Scripted {
    fn new(replies: &[&str]) -> Self {
        Self {
            replies: replies.iter().map(|line| (*line).to_owned()).collect(),
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
            return Err("the plugin said nothing further".to_owned());
        }
        Ok(self.replies.remove(0))
    }

    fn receive_exact(&mut self, _length: usize) -> Result<Vec<u8>, String> {
        Err("this conversation carries no content run".to_owned())
    }
}

const MANIFEST: &str = r#"{
  "manifest_version": 1,
  "name": "org.example.viewer",
  "version": "1.0.0",
  "nostdb": ">=0.1.0 <0.2.0",
  "entrypoint": { "command": ["bin/viewer"] },
  "protocol_version": 1,
  "actions": [{ "name": "view", "ai_usage": "none" }],
  "permissions": {
    "graph_read": true,
    "database_write": false,
    "output_paths": [".nostdb/out/**"],
    "network_hosts": []
  }
}
"#;

/// Installs a plugin the way the manager would have, and records it.
fn install(root: &Path, manifest: &str) -> Record {
    let name = "org.example.viewer";
    let directory = plugins_directory(root).join(name);
    std::fs::create_dir_all(directory.join("bin")).expect("plugin directory");
    std::fs::write(directory.join("nostdb-plugin.json"), manifest).expect("manifest");
    std::fs::write(directory.join("bin").join("viewer"), b"binary").expect("entrypoint");

    let (manifest_digest, tree_digest) = recompute_digests(&directory).expect("digests");
    let parsed: serde_json::Value = serde_json::from_str(manifest).expect("manifest is JSON");
    let permissions = parsed["permissions"].clone();

    let mut record = Record::default();
    record.upsert(Installation {
        name: name.to_owned(),
        repository: "https://github.com/example/viewer".to_owned(),
        commit: "a".repeat(40),
        subdirectory: None,
        manifest_digest,
        tree_digest,
        scope: Scope::Project,
        manifest_version: 1,
        plugin_version: "1.0.0".to_owned(),
        approved_permissions: permissions,
    });
    write_record(&record, root).expect("record written");
    record
}

fn engine() -> nostdb_cli::plugin_install::Version {
    nostdb_cli::plugin_install::Version::parse("0.1.0").expect("a version")
}

fn ready(root: &Path, record: &Record, action: &str) -> Result<Ready, String> {
    preflight(
        record,
        Scope::Project,
        root,
        "org.example.viewer",
        action,
        &engine(),
    )
    .map_err(|error| error.code)
}

#[test]
fn an_installed_plugin_passes_every_check_and_completes() {
    let scratch = Scratch::new("complete");
    let record = install(scratch.path(), MANIFEST);
    let ready = ready(scratch.path(), &record, "view").expect("ready");

    assert_eq!(ready.command, ["bin/viewer"]);
    assert_eq!(ready.declared_actions, ["view"]);
    assert!(ready.graph_read);
    assert_eq!(ready.output_paths, [".nostdb/out/**"]);

    let mut transport = Scripted::new(&[
        r#"{"plugin_protocol_version":1,"reply":"handshake","plugin":"org.example.viewer","plugin_version":"1.0.0","actions":["view"]}"#,
        r#"{"plugin_protocol_version":1,"reply":"invoke","status":"complete","outputs":["view.html"]}"#,
    ]);
    let (handshake, invoked) = converse(
        &mut transport,
        &ready,
        "view",
        Path::new("/project/.nostdb/out"),
        None,
    )
    .expect("completed");

    assert_eq!(handshake.plugin, "org.example.viewer");
    assert!(invoked.is_complete());
    assert_eq!(invoked.outputs, ["view.html"]);

    // The handshake is first, and nothing else is sent until it has been answered.
    assert_eq!(transport.sent.len(), 2);
    assert!(transport.sent[0].contains("\"request\":\"handshake\""));
    assert!(transport.sent[1].contains("\"request\":\"invoke\""));
    // Line-delimited framing, so a request carrying a newline would be two messages.
    assert!(transport.sent.iter().all(|line| !line.contains('\n')));
}

#[test]
fn a_file_added_to_the_installed_directory_refuses_the_next_run() {
    let scratch = Scratch::new("tampered-tree");
    let record = install(scratch.path(), MANIFEST);
    assert!(ready(scratch.path(), &record, "view").is_ok());

    // A plugin that used its own directory as scratch space, or somebody who edited it.
    let directory = plugins_directory(scratch.path()).join("org.example.viewer");
    std::fs::write(directory.join("extra.bin"), b"added later").expect("extra file");

    assert_eq!(
        ready(scratch.path(), &record, "view").expect_err("refused"),
        "PLUGIN_DIGEST_MISMATCH"
    );
}

#[test]
fn an_edited_manifest_refuses_the_next_run_and_says_the_manifest_moved() {
    let scratch = Scratch::new("tampered-manifest");
    let record = install(scratch.path(), MANIFEST);

    // The permission a user declined, granted by editing the file the plugin ships.
    let widened = MANIFEST.replace(
        r#""output_paths": [".nostdb/out/**"]"#,
        r#""output_paths": ["**"]"#,
    );
    let directory = plugins_directory(scratch.path()).join("org.example.viewer");
    std::fs::write(directory.join("nostdb-plugin.json"), &widened).expect("edited manifest");

    let error = preflight(
        &record,
        Scope::Project,
        scratch.path(),
        "org.example.viewer",
        "view",
        &engine(),
    )
    .expect_err("refused");
    assert_eq!(error.code, "PLUGIN_DIGEST_MISMATCH");
    assert!(
        error.reason.contains("manifest changed"),
        "{}",
        error.reason
    );
}

#[test]
fn a_widened_manifest_never_takes_effect_even_though_it_is_on_disk() {
    // The point of the record: the approval is the authority. This proves the widened permission
    // does not reach a `Ready`, because the run is refused before the manifest is read at all.
    let scratch = Scratch::new("approval-wins");
    let record = install(scratch.path(), MANIFEST);
    let directory = plugins_directory(scratch.path()).join("org.example.viewer");

    let widened = MANIFEST.replace(
        r#""actions": [{ "name": "view", "ai_usage": "none" }]"#,
        r#""actions": [{ "name": "view", "ai_usage": "none" }, { "name": "exfiltrate", "ai_usage": "none" }]"#,
    );
    std::fs::write(directory.join("nostdb-plugin.json"), &widened).expect("edited manifest");

    assert_eq!(
        ready(scratch.path(), &record, "exfiltrate").expect_err("refused"),
        "PLUGIN_DIGEST_MISMATCH",
        "an action added by editing the manifest must not become invocable"
    );
}

#[test]
fn an_action_the_approved_manifest_never_declared_is_refused_before_the_process_starts() {
    let scratch = Scratch::new("unknown-action");
    let record = install(scratch.path(), MANIFEST);
    assert_eq!(
        ready(scratch.path(), &record, "exfiltrate").expect_err("refused"),
        ProtocolCode::ActionUnknown.as_str()
    );
}

#[test]
fn an_engine_outside_the_declared_range_is_refused() {
    let scratch = Scratch::new("incompatible");
    let record = install(scratch.path(), MANIFEST);
    let error = preflight(
        &record,
        Scope::Project,
        scratch.path(),
        "org.example.viewer",
        "view",
        &nostdb_cli::plugin_install::Version::parse("9.0.0").expect("a version"),
    )
    .expect_err("refused");
    assert_eq!(error.code, "PLUGIN_INCOMPATIBLE");
}

#[test]
fn a_plugin_that_is_not_recorded_is_required() {
    let scratch = Scratch::new("not-recorded");
    install(scratch.path(), MANIFEST);
    // The files are there and the record is empty: a directory somebody copied in is not an
    // installation, which is the one thing the record exists to distinguish.
    let empty = Record::default();
    let error = preflight(
        &empty,
        Scope::Project,
        scratch.path(),
        "org.example.viewer",
        "view",
        &engine(),
    )
    .expect_err("refused");
    assert_eq!(error.code, ProtocolCode::Required.as_str());
}

#[test]
fn a_symbolic_link_in_the_installed_directory_is_refused_rather_than_followed() {
    let scratch = Scratch::new("symlink");
    let record = install(scratch.path(), MANIFEST);
    let directory = plugins_directory(scratch.path()).join("org.example.viewer");

    let outside = scratch.path().join("secret");
    std::fs::write(&outside, b"not part of any plugin").expect("a file outside");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, directory.join("link")).expect("symlink");
    #[cfg(not(unix))]
    return;

    // Following it would digest a file outside the plugin, and an installation never wrote one.
    assert_eq!(
        ready(scratch.path(), &record, "view").expect_err("refused"),
        ProtocolCode::Failed.as_str()
    );
    let _ = record;
}

#[test]
fn a_plugin_claiming_an_unapproved_action_is_refused_after_the_handshake() {
    let scratch = Scratch::new("lying-handshake");
    let record = install(scratch.path(), MANIFEST);
    let ready = ready(scratch.path(), &record, "view").expect("ready");

    let mut transport = Scripted::new(&[
        r#"{"plugin_protocol_version":1,"reply":"handshake","plugin":"org.example.viewer","plugin_version":"1.0.0","actions":["view","exfiltrate"]}"#,
    ]);
    let error = converse(
        &mut transport,
        &ready,
        "view",
        Path::new("/project/.nostdb/out"),
        None,
    )
    .expect_err("refused");

    assert_eq!(error.code, ProtocolCode::IdentityMismatch.as_str());
    // Refused before anything was invoked, so a plugin that claimed more never got to act on it.
    assert_eq!(transport.sent.len(), 1);
}

#[test]
fn an_output_outside_the_approval_fails_the_invocation() {
    let scratch = Scratch::new("bad-output");
    let record = install(scratch.path(), MANIFEST);
    let ready = ready(scratch.path(), &record, "view").expect("ready");

    let mut transport = Scripted::new(&[
        r#"{"plugin_protocol_version":1,"reply":"handshake","plugin":"org.example.viewer","plugin_version":"1.0.0","actions":["view"]}"#,
        r#"{"plugin_protocol_version":1,"reply":"invoke","status":"complete","outputs":["../../escaped.html"]}"#,
    ]);
    let error = converse(
        &mut transport,
        &ready,
        "view",
        Path::new("/project/.nostdb/out"),
        None,
    )
    .expect_err("refused");
    assert_eq!(error.code, ProtocolCode::Failed.as_str());
}

#[test]
fn a_plugin_that_says_nothing_is_a_failure_rather_than_a_success() {
    let scratch = Scratch::new("silent");
    let record = install(scratch.path(), MANIFEST);
    let ready = ready(scratch.path(), &record, "view").expect("ready");

    let mut transport = Scripted::new(&[]);
    let error = converse(
        &mut transport,
        &ready,
        "view",
        Path::new("/project/.nostdb/out"),
        None,
    )
    .expect_err("refused");
    assert_eq!(error.code, ProtocolCode::Failed.as_str());
}

#[test]
fn the_recomputed_manifest_digest_is_the_digest_of_the_installed_bytes() {
    let scratch = Scratch::new("digest-shape");
    install(scratch.path(), MANIFEST);
    let directory = plugins_directory(scratch.path()).join("org.example.viewer");
    let (manifest, tree) = recompute_digests(&directory).expect("computed");

    // Exactly the bytes on disk, with no reserialization: two documents that differ only in
    // formatting must not digest the same, and one that differs in content must not digest alike.
    assert_eq!(manifest, digest_bytes(MANIFEST.as_bytes()).to_string());
    assert_ne!(manifest, tree);
}
