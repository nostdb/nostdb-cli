//! The command surface, driven end to end against real files.
//!
//! Every case runs `nostdb_cli::run` in-process with captured writers, so it asserts the
//! exit class and the two streams together. That pairing is the point: a command that
//! reports the right class while printing a diagnostic to stdout has still broken the
//! contract that machine-readable output carries no commentary.

use nostdb_cli::{ExitClass, run};
use std::fs;
use std::path::{Path, PathBuf};

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let mut base = std::env::temp_dir();
        base.push(format!("nostdb-cli-{label}"));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("temporary directory");
        Self(base)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Output {
    class: ExitClass,
    out: String,
    err: String,
}

fn nostdb<const N: usize>(arguments: [&str; N]) -> Output {
    let owned: Vec<String> = arguments.iter().map(|a| (*a).to_owned()).collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let class = run(&owned, &mut out, &mut err);
    Output {
        class,
        out: String::from_utf8(out).expect("stdout is UTF-8"),
        err: String::from_utf8(err).expect("stderr is UTF-8"),
    }
}

const SAMPLE: &str = "\
@nost 2

schema Function {
  name: string,
}

node login: Function {
  id: \"n_0198a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b\",
  name: \"login\",
}

node other: Function {
  id: \"n_0198a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5c\",
  name: \"other\",
}

edge login -> other :CALLS {
  id: \"e_0198a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5d\",
}
";

#[test]
fn help_and_version_succeed_and_write_to_stdout() {
    for arguments in [
        vec!["help"],
        vec!["help", "convert"],
        vec!["--version"],
        vec!["--version", "--json"],
    ] {
        let owned: Vec<String> = arguments.iter().map(|a| (*a).to_owned()).collect();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let class = run(&owned, &mut out, &mut err);
        assert_eq!(class, ExitClass::Success, "{arguments:?}");
        assert!(!out.is_empty(), "{arguments:?} wrote nothing to stdout");
        assert!(err.is_empty(), "{arguments:?} wrote to stderr");
    }
}

#[test]
fn a_bare_invocation_shows_the_summary_rather_than_failing() {
    let result = nostdb([]);
    assert_eq!(result.class, ExitClass::Success);
    assert!(result.out.contains("Usage:"), "{}", result.out);
}

#[test]
fn a_usage_mistake_exits_two_with_nothing_on_stdout() {
    for arguments in [
        vec!["frobnicate"],
        vec!["check"],
        vec!["convert", "only-one.nost"],
        vec!["export"],
        vec!["--version", "--xml"],
    ] {
        let owned: Vec<String> = arguments.iter().map(|a| (*a).to_owned()).collect();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let class = run(&owned, &mut out, &mut err);
        assert_eq!(class, ExitClass::Usage, "{arguments:?}");
        assert!(out.is_empty(), "{arguments:?} wrote to stdout");
        assert!(!err.is_empty(), "{arguments:?} explained nothing");
    }
}

#[test]
fn init_creates_a_project_and_refuses_a_second_time() {
    let dir = TempDir::new("init");
    let root = dir.path().to_string_lossy().into_owned();

    let first = nostdb(["init", &root]);
    assert_eq!(first.class, ExitClass::Success, "{}", first.err);
    assert!(dir.join(".nostdb/settings.json").is_file());
    assert!(dir.join(".nostdb/root.nostdb").is_file());
    // The path a caller would pipe is the only thing on stdout.
    assert_eq!(first.out.trim(), dir.path().to_string_lossy());

    let second = nostdb(["init", &root]);
    assert_eq!(second.class, ExitClass::Usage);
    assert!(second.err.contains("already a configured project"));
}

#[test]
fn check_accepts_a_valid_nost_file_and_the_database_it_becomes() {
    let dir = TempDir::new("check-valid");
    let nost = dir.join("root.nost");
    let database = dir.join("root.nostdb");
    fs::write(&nost, SAMPLE).unwrap();

    let checked = nostdb(["check", nost.to_str().unwrap()]);
    assert_eq!(checked.class, ExitClass::Success, "{}", checked.err);
    assert!(checked.out.contains("valid"));

    let converted = nostdb([
        "convert",
        nost.to_str().unwrap(),
        database.to_str().unwrap(),
    ]);
    assert_eq!(converted.class, ExitClass::Success, "{}", converted.err);

    let rechecked = nostdb(["check", database.to_str().unwrap()]);
    assert_eq!(rechecked.class, ExitClass::Success, "{}", rechecked.err);
    assert!(rechecked.out.contains("2 nodes"), "{}", rechecked.out);
    assert!(rechecked.out.contains("1 edges"), "{}", rechecked.out);
}

#[test]
fn check_reports_a_syntax_error_as_a_validation_failure() {
    let dir = TempDir::new("check-syntax");
    let nost = dir.join("broken.nost");
    fs::write(&nost, "@nost 2\nnode n {\n}\n").unwrap();

    let result = nostdb(["check", nost.to_str().unwrap()]);
    assert_eq!(result.class, ExitClass::Validation);
    assert!(result.out.is_empty(), "{}", result.out);
    assert!(result.err.contains("NOST_PARSE_ERROR"), "{}", result.err);
}

#[test]
fn check_reports_a_semantic_error_with_its_code() {
    let dir = TempDir::new("check-semantic");
    let nost = dir.join("duplicate.nost");
    fs::write(&nost, "@nost 2\nnode a: L {}\nnode a: L {}\n").unwrap();

    let result = nostdb(["check", nost.to_str().unwrap()]);
    assert_eq!(result.class, ExitClass::Validation);
    assert!(
        result.err.contains("NOST_DUPLICATE_DECLARATION_NAME"),
        "{}",
        result.err
    );
}

#[test]
fn a_warning_alone_still_succeeds_and_is_still_reported() {
    // Exit class 0 covers "success with non-strict warnings", and a warning a caller
    // never sees is the same as no warning at all.
    let dir = TempDir::new("check-warning");
    let nost = dir.join("warned.nost");
    fs::write(&nost, "@nost 2\nnode a: L {}\nedge a -> gone :R {}\n").unwrap();

    let result = nostdb(["check", nost.to_str().unwrap()]);
    assert_eq!(result.class, ExitClass::Success, "{}", result.err);
    assert!(
        result.err.contains("NOST_UNRESOLVED_ENDPOINT"),
        "{}",
        result.err
    );
    assert!(result.out.contains("valid"));
}

#[test]
fn check_refuses_a_file_it_cannot_classify() {
    let dir = TempDir::new("check-unknown");
    let other = dir.join("notes.txt");
    fs::write(&other, "hello").unwrap();

    let result = nostdb(["check", other.to_str().unwrap()]);
    assert_eq!(result.class, ExitClass::Usage);
    assert!(result.err.contains(".nost"), "{}", result.err);
}

#[test]
fn check_reports_a_missing_file_as_an_io_failure() {
    let dir = TempDir::new("check-missing");
    let absent = dir.join("absent.nost");
    let result = nostdb(["check", absent.to_str().unwrap()]);
    assert_eq!(result.class, ExitClass::Io);
    assert!(result.out.is_empty());
}

#[test]
fn convert_round_trips_a_document_through_a_database() {
    let dir = TempDir::new("convert-round-trip");
    let source = dir.join("source.nost");
    let database = dir.join("root.nostdb");
    let back = dir.join("back.nost");
    fs::write(&source, SAMPLE).unwrap();

    assert_eq!(
        nostdb([
            "convert",
            source.to_str().unwrap(),
            database.to_str().unwrap()
        ])
        .class,
        ExitClass::Success
    );
    assert_eq!(
        nostdb([
            "convert",
            database.to_str().unwrap(),
            back.to_str().unwrap()
        ])
        .class,
        ExitClass::Success
    );

    // Exporting again reproduces the same text, which is the fixed point the conversion
    // guarantees. The first pass may rename declarations, because a declaration name is
    // file-local rather than graph data.
    let again = dir.join("again.nostdb");
    let twice = dir.join("twice.nost");
    assert_eq!(
        nostdb(["convert", back.to_str().unwrap(), again.to_str().unwrap()]).class,
        ExitClass::Success
    );
    assert_eq!(
        nostdb(["convert", again.to_str().unwrap(), twice.to_str().unwrap()]).class,
        ExitClass::Success
    );
    assert_eq!(
        fs::read_to_string(&back).unwrap(),
        fs::read_to_string(&twice).unwrap()
    );
}

#[test]
fn a_refused_conversion_leaves_the_target_exactly_as_it_was() {
    let dir = TempDir::new("convert-preserves");
    let broken = dir.join("broken.nost");
    let target = dir.join("root.nostdb");
    fs::write(&broken, "@nost 2\nnode a: L {\n id: \"n_1\",\n}\n").unwrap();

    // Seed the target with a database built from something valid.
    let good = dir.join("good.nost");
    fs::write(&good, SAMPLE).unwrap();
    assert_eq!(
        nostdb(["convert", good.to_str().unwrap(), target.to_str().unwrap()]).class,
        ExitClass::Success
    );
    let before = fs::read(&target).unwrap();

    let refused = nostdb([
        "convert",
        broken.to_str().unwrap(),
        target.to_str().unwrap(),
    ]);
    assert_eq!(refused.class, ExitClass::Validation);
    assert!(refused.err.contains("NOST_INVALID_ID"), "{}", refused.err);
    assert_eq!(
        fs::read(&target).unwrap(),
        before,
        "a refused conversion must not touch the target"
    );

    // And no staging file is left behind.
    let leftovers: Vec<PathBuf> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().ends_with(".staged"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn convert_refuses_an_external_endpoint_as_unavailable_rather_than_invalid() {
    // Link resolution is a later increment. The document is well formed, so refusing it
    // as a validation error would say the file is wrong when the build is incomplete.
    let dir = TempDir::new("convert-external");
    let source = dir.join("linked.nost");
    let target = dir.join("root.nostdb");
    fs::write(
        &source,
        "@nost 2\n@link \"./shared\" as shared\nnode a: L {}\nedge a -> shared::x :R {}\n",
    )
    .unwrap();

    let result = nostdb([
        "convert",
        source.to_str().unwrap(),
        target.to_str().unwrap(),
    ]);
    assert_eq!(result.class, ExitClass::Unavailable);
    assert!(result.err.contains("link resolution"), "{}", result.err);
    assert!(!target.exists(), "nothing should have been written");
}

#[test]
fn convert_refuses_a_direction_that_is_not_one() {
    let dir = TempDir::new("convert-direction");
    let source = dir.join("a.nost");
    fs::write(&source, SAMPLE).unwrap();

    let copy = nostdb([
        "convert",
        source.to_str().unwrap(),
        dir.join("b.nost").to_str().unwrap(),
    ]);
    assert_eq!(copy.class, ExitClass::Usage);
    assert!(copy.err.contains("copy"), "{}", copy.err);

    let unknown = nostdb([
        "convert",
        source.to_str().unwrap(),
        dir.join("b.txt").to_str().unwrap(),
    ]);
    assert_eq!(unknown.class, ExitClass::Usage);
}

#[test]
fn export_writes_the_project_graph_and_warns_when_materialization_is_off() {
    let dir = TempDir::new("export");
    let root = dir.path().to_string_lossy().into_owned();
    assert_eq!(nostdb(["init", &root]).class, ExitClass::Success);

    // Put something in the database by converting into it.
    let source = dir.join("source.nost");
    fs::write(&source, SAMPLE).unwrap();
    assert_eq!(
        nostdb([
            "convert",
            source.to_str().unwrap(),
            dir.join(".nostdb/root.nostdb").to_str().unwrap()
        ])
        .class,
        ExitClass::Success
    );

    let exported = nostdb(["export", "--nost", &root]);
    assert_eq!(exported.class, ExitClass::Success, "{}", exported.err);
    let written = dir.join(".nostdb/root.nost");
    assert!(written.is_file());
    assert!(fs::read_to_string(&written).unwrap().starts_with("@nost 2"));
    // database.nost defaults to false, so the file is written and the caller is told it
    // will not be maintained.
    assert!(
        exported.err.contains("database.nost is false"),
        "{}",
        exported.err
    );
    assert_eq!(exported.out.trim(), written.to_string_lossy());
}

#[test]
fn export_finds_the_project_from_a_nested_directory() {
    let dir = TempDir::new("export-nested");
    let root = dir.path().to_string_lossy().into_owned();
    assert_eq!(nostdb(["init", &root]).class, ExitClass::Success);
    let nested = dir.join("packages/child");
    fs::create_dir_all(&nested).unwrap();

    let exported = nostdb(["export", "--nost", nested.to_str().unwrap()]);
    assert_eq!(exported.class, ExitClass::Success, "{}", exported.err);
    assert!(dir.join(".nostdb/root.nost").is_file());
}

#[test]
fn export_outside_a_project_is_a_usage_error_naming_the_remedy() {
    let dir = TempDir::new("export-unconfigured");
    let result = nostdb(["export", "--nost", dir.path().to_str().unwrap()]);
    assert_eq!(result.class, ExitClass::Usage);
    assert!(result.err.contains("nostdb init"), "{}", result.err);
}

#[test]
fn export_reports_an_orphan_settings_entry_without_refusing() {
    let dir = TempDir::new("export-orphan");
    let root = dir.path().to_string_lossy().into_owned();
    assert_eq!(nostdb(["init", &root]).class, ExitClass::Success);
    fs::write(
        dir.join(".nostdb/settings.json"),
        "{\"settings_version\": 1, \"links\": [{\"source\": \"./gone\"}]}",
    )
    .unwrap();

    let result = nostdb(["export", "--nost", &root]);
    assert_eq!(result.class, ExitClass::Success, "{}", result.err);
    assert!(
        result.err.contains("ORPHAN_LINK_SETTINGS"),
        "{}",
        result.err
    );
    assert!(dir.join(".nostdb/root.nost").is_file());
}

#[test]
fn a_refused_settings_document_is_a_validation_failure_naming_the_file() {
    let dir = TempDir::new("export-bad-settings");
    let root = dir.path().to_string_lossy().into_owned();
    assert_eq!(nostdb(["init", &root]).class, ExitClass::Success);
    fs::write(
        dir.join(".nostdb/settings.json"),
        "{\"settings_version\": 1, \"links\": [{\"source\": \"./a\", \"alias\": \"a\"}]}",
    )
    .unwrap();

    let result = nostdb(["export", "--nost", &root]);
    assert_eq!(result.class, ExitClass::Validation);
    assert!(result.err.contains("settings.json"), "{}", result.err);
    assert!(result.err.contains("alias"), "{}", result.err);
}

// -- query, the output formats, and the REPL -------------------------------------

/// A configured project holding two Function nodes and one edge between them.
fn seeded_project(label: &str) -> TempDir {
    let dir = TempDir::new(label);
    let root = dir.path().to_string_lossy().into_owned();
    assert_eq!(nostdb(["init", &root]).class, ExitClass::Success);
    let source = dir.join("seed.nost");
    fs::write(&source, SAMPLE).unwrap();
    assert_eq!(
        nostdb([
            "convert",
            source.to_str().unwrap(),
            dir.join(".nostdb/root.nostdb").to_str().unwrap()
        ])
        .class,
        ExitClass::Success
    );
    dir
}

#[test]
fn query_reads_in_every_format_and_only_json_carries_the_warnings() {
    let dir = seeded_project("query-formats");
    let root = dir.path().to_string_lossy().into_owned();
    let statement = "MATCH (n:Function) RETURN n.name ORDER BY n.name";

    let json = nostdb(["query", statement, "--format", "json", "--project", &root]);
    assert_eq!(json.class, ExitClass::Success, "{}", json.err);
    let parsed: serde_json::Value = serde_json::from_str(&json.out).expect("one JSON document");
    assert_eq!(parsed["result_version"], 1);
    assert_eq!(parsed["summary"]["rows"], 2);
    assert_eq!(parsed["rows"][0][0], "login");
    // A read reports no write summary at all.
    assert!(parsed["summary"].get("writes").is_none(), "{}", json.out);

    let jsonl = nostdb(["query", statement, "--format", "jsonl", "--project", &root]);
    assert_eq!(jsonl.class, ExitClass::Success);
    let lines: Vec<&str> = jsonl.out.lines().collect();
    assert_eq!(lines.len(), 4, "{}", jsonl.out);
    for line in &lines {
        serde_json::from_str::<serde_json::Value>(line).expect("every line is JSON");
    }

    let csv = nostdb(["query", statement, "--format", "csv", "--project", &root]);
    assert_eq!(csv.class, ExitClass::Success);
    assert_eq!(csv.out.lines().next(), Some("n.name"));
    assert_eq!(csv.out.lines().nth(1), Some("login"));

    let table = nostdb(["query", statement, "--format", "table", "--project", &root]);
    assert_eq!(table.class, ExitClass::Success);
    assert!(table.out.contains("2 rows"), "{}", table.out);
}

#[test]
fn the_default_format_is_the_table() {
    let dir = seeded_project("query-default-format");
    let root = dir.path().to_string_lossy().into_owned();
    let result = nostdb(["query", "MATCH (n) RETURN n.name", "--project", &root]);
    assert_eq!(result.class, ExitClass::Success, "{}", result.err);
    assert!(result.out.contains("row"), "{}", result.out);
    assert!(
        serde_json::from_str::<serde_json::Value>(&result.out).is_err(),
        "the default must not be JSON"
    );
}

#[test]
fn a_write_reports_what_it_changed_and_the_change_survives() {
    let dir = seeded_project("query-write");
    let root = dir.path().to_string_lossy().into_owned();

    let written = nostdb([
        "query",
        "CREATE (n:Function {name: \"added\"})",
        "--format",
        "json",
        "--project",
        &root,
    ]);
    assert_eq!(written.class, ExitClass::Success, "{}", written.err);
    let parsed: serde_json::Value = serde_json::from_str(&written.out).unwrap();
    assert_eq!(parsed["summary"]["writes"]["nodes_created"], 1);

    let counted = nostdb([
        "query",
        "MATCH (n:Function) RETURN n.name ORDER BY n.name",
        "--format",
        "json",
        "--project",
        &root,
    ]);
    let parsed: serde_json::Value = serde_json::from_str(&counted.out).unwrap();
    assert_eq!(parsed["summary"]["rows"], 3, "the write was committed");
}

#[test]
fn unsupported_syntax_is_a_usage_failure_and_a_semantic_error_is_validation() {
    let dir = seeded_project("query-refusals");
    let root = dir.path().to_string_lossy().into_owned();

    // Outside the published subset: the remedy is to write something else.
    let unsupported = nostdb([
        "query",
        "CREATE INDEX ON :Function(name)",
        "--project",
        &root,
    ]);
    assert_eq!(unsupported.class, ExitClass::Usage);
    assert!(
        unsupported.err.contains("CYPHER_UNSUPPORTED"),
        "{}",
        unsupported.err
    );
    assert!(unsupported.out.is_empty());

    // Inside the subset and wrong.
    let semantic = nostdb(["query", "RETURN missing.name", "--project", &root]);
    assert_eq!(semantic.class, ExitClass::Validation);
    assert!(
        semantic.err.contains("CYPHER_SEMANTIC_ERROR"),
        "{}",
        semantic.err
    );
}

#[test]
fn query_outside_a_project_is_a_usage_error() {
    let dir = TempDir::new("query-unconfigured");
    let result = nostdb([
        "query",
        "RETURN 1",
        "--project",
        dir.path().to_str().unwrap(),
    ]);
    assert_eq!(result.class, ExitClass::Usage);
    assert!(result.err.contains("nostdb init"), "{}", result.err);
}

#[test]
fn an_unknown_format_names_the_ones_that_exist() {
    let result = nostdb(["query", "RETURN 1", "--format", "yaml"]);
    assert_eq!(result.class, ExitClass::Usage);
    assert!(result.err.contains("yaml"), "{}", result.err);
    assert!(result.err.contains("json"), "{}", result.err);
}

#[test]
fn a_format_option_may_be_written_either_way_and_may_follow_the_statement() {
    let dir = seeded_project("query-option-forms");
    let root = dir.path().to_string_lossy().into_owned();
    for arguments in [
        vec!["query", "--format", "json", "--project", &root, "RETURN 1"],
        vec!["query", "RETURN 1", "--format=json", "--project", &root],
    ] {
        let owned: Vec<String> = arguments.iter().map(|a| (*a).to_owned()).collect();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let class = run(&owned, &mut out, &mut err);
        assert_eq!(class, ExitClass::Success, "{arguments:?}: {err:?}");
        serde_json::from_str::<serde_json::Value>(&String::from_utf8(out).unwrap())
            .expect("JSON was asked for");
    }
}

// -- link ------------------------------------------------------------------------

#[test]
fn link_list_reports_no_links_for_a_fresh_project() {
    let dir = TempDir::new("link-empty");
    let root = dir.path().to_string_lossy().into_owned();
    assert_eq!(nostdb(["init", &root]).class, ExitClass::Success);

    let listed = nostdb(["link", "list", "--project", &root]);
    assert_eq!(listed.class, ExitClass::Success, "{}", listed.err);
    assert!(listed.out.contains("no links declared"), "{}", listed.out);

    let checked = nostdb(["link", "check", "--project", &root]);
    assert_eq!(checked.class, ExitClass::Success);
}

/// A project whose database declares the given links, written through `.nost`.
fn project_with_links(label: &str, links: &str) -> TempDir {
    let dir = TempDir::new(label);
    let root = dir.path().to_string_lossy().into_owned();
    assert_eq!(nostdb(["init", &root]).class, ExitClass::Success);
    let source = dir.join("seed.nost");
    fs::write(&source, format!("@nost 2\n\n{links}\nnode a: L {{}}\n")).unwrap();
    let converted = nostdb([
        "convert",
        source.to_str().unwrap(),
        dir.join(".nostdb/root.nostdb").to_str().unwrap(),
    ]);
    assert_eq!(converted.class, ExitClass::Success, "{}", converted.err);
    dir
}

#[test]
fn link_list_succeeds_over_a_broken_link_and_check_does_not() {
    let dir = project_with_links("link-broken", "@link \"./absent.nostdb\"\n");
    let root = dir.path().to_string_lossy().into_owned();

    // list reports and does not judge.
    let listed = nostdb(["link", "list", "--project", &root]);
    assert_eq!(listed.class, ExitClass::Success, "{}", listed.err);
    assert!(listed.out.contains("LINK_UNAVAILABLE"), "{}", listed.out);
    assert!(listed.err.contains("LINK_UNAVAILABLE"), "{}", listed.err);

    // check is the one that judges.
    let checked = nostdb(["link", "check", "--project", &root]);
    assert_eq!(checked.class, ExitClass::Unavailable);
    assert!(checked.err.contains("LINK_UNAVAILABLE"), "{}", checked.err);
}

#[test]
fn link_list_reports_a_reachable_link_as_opened() {
    let target = TempDir::new("link-target");
    let target_root = target.path().to_string_lossy().into_owned();
    assert_eq!(nostdb(["init", &target_root]).class, ExitClass::Success);

    let dir = TempDir::new("link-reachable");
    let root = dir.path().to_string_lossy().into_owned();
    assert_eq!(nostdb(["init", &root]).class, ExitClass::Success);
    let source = dir.join("seed.nost");
    // The locator resolves from the database's directory, which is `.nostdb`.
    fs::write(
        &source,
        format!(
            "@nost 2\n\n@link \"{}\" as target\n\nnode a: L {{}}\n",
            target.path().join(".nostdb/root.nostdb").display()
        ),
    )
    .unwrap();
    assert_eq!(
        nostdb([
            "convert",
            source.to_str().unwrap(),
            dir.join(".nostdb/root.nostdb").to_str().unwrap()
        ])
        .class,
        ExitClass::Success
    );

    let listed = nostdb(["link", "list", "--format", "json", "--project", &root]);
    assert_eq!(listed.class, ExitClass::Success, "{}", listed.err);
    let parsed: serde_json::Value = serde_json::from_str(&listed.out).expect("JSON");
    assert_eq!(
        parsed["summary"]["linked_databases_opened"], 1,
        "{}",
        listed.out
    );
    assert_eq!(parsed["summary"]["partial"], false);
    assert_eq!(parsed["links"][0]["available"], true);
    assert_eq!(parsed["links"][0]["alias"], "target");

    assert_eq!(
        nostdb(["link", "check", "--project", &root]).class,
        ExitClass::Success
    );
}

#[test]
fn a_remote_link_says_there_is_no_provider() {
    let dir = project_with_links(
        "link-remote",
        "@link \"github://example/shared/root.nostdb?ref=main\"\n",
    );
    let root = dir.path().to_string_lossy().into_owned();
    let listed = nostdb(["link", "list", "--format", "json", "--project", &root]);
    assert_eq!(listed.class, ExitClass::Success, "{}", listed.err);
    let parsed: serde_json::Value = serde_json::from_str(&listed.out).unwrap();
    assert_eq!(parsed["links"][0]["code"], "LINK_UNAVAILABLE");
    assert!(
        parsed["links"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("provider"),
        "{}",
        listed.out
    );
}

#[test]
fn a_deferred_link_action_says_it_is_not_built_rather_than_unknown() {
    // A caller who typed a real command deserves to be told it is not built yet.
    for action in ["add", "remove", "refresh"] {
        let result = nostdb(["link", action]);
        assert_eq!(result.class, ExitClass::Usage, "{action}");
        assert!(
            result.err.contains("not implemented yet"),
            "{action}: {}",
            result.err
        );
        assert!(result.err.contains("journal"), "{action}: {}", result.err);
    }
}

#[test]
fn an_unknown_link_action_lists_the_ones_that_exist() {
    let result = nostdb(["link", "frobnicate"]);
    assert_eq!(result.class, ExitClass::Usage);
    assert!(result.err.contains("list"), "{}", result.err);
    assert!(result.err.contains("check"), "{}", result.err);

    let missing = nostdb(["link"]);
    assert_eq!(missing.class, ExitClass::Usage);
    assert!(missing.err.contains("needs an action"), "{}", missing.err);
}

#[test]
fn link_reports_an_orphan_settings_entry_without_changing_the_exit_class() {
    let dir = TempDir::new("link-orphan");
    let root = dir.path().to_string_lossy().into_owned();
    assert_eq!(nostdb(["init", &root]).class, ExitClass::Success);
    fs::write(
        dir.join(".nostdb/settings.json"),
        "{\"settings_version\": 1, \"links\": [{\"source\": \"./gone\"}]}",
    )
    .unwrap();

    let checked = nostdb(["link", "check", "--project", &root]);
    assert_eq!(
        checked.class,
        ExitClass::Success,
        "an orphan is about settings, not reachability"
    );
    assert!(
        checked.err.contains("ORPHAN_LINK_SETTINGS"),
        "{}",
        checked.err
    );
}

#[test]
fn a_query_sees_records_from_a_linked_source() {
    // The gap this closes: `link list` reported an opened source while `query` saw none.
    let target = TempDir::new("federated-target");
    let target_root = target.path().to_string_lossy().into_owned();
    assert_eq!(nostdb(["init", &target_root]).class, ExitClass::Success);
    assert_eq!(
        nostdb([
            "query",
            "CREATE (n:Function {name: \"from-child\"})",
            "--project",
            &target_root
        ])
        .class,
        ExitClass::Success
    );

    let dir = TempDir::new("federated-root");
    let root = dir.path().to_string_lossy().into_owned();
    assert_eq!(nostdb(["init", &root]).class, ExitClass::Success);
    let source = dir.join("seed.nost");
    fs::write(
        &source,
        format!(
            "@nost 2\n\n@link \"{}\" as child\n\nnode local: Function {{\n  name: \"from-root\",\n}}\n",
            target.path().join(".nostdb/root.nostdb").display()
        ),
    )
    .unwrap();
    assert_eq!(
        nostdb([
            "convert",
            source.to_str().unwrap(),
            dir.join(".nostdb/root.nostdb").to_str().unwrap()
        ])
        .class,
        ExitClass::Success
    );

    let result = nostdb([
        "query",
        "MATCH (n:Function) RETURN n.name, nostdb.source(n) ORDER BY n.name",
        "--format",
        "json",
        "--project",
        &root,
    ]);
    assert_eq!(result.class, ExitClass::Success, "{}", result.err);
    let parsed: serde_json::Value = serde_json::from_str(&result.out).expect("JSON");

    assert_eq!(parsed["summary"]["rows"], 2, "{}", result.out);
    assert_eq!(parsed["summary"]["linked_databases_opened"], 1);
    assert_eq!(parsed["summary"]["partial"], false);
    assert_eq!(parsed["rows"][0][0], "from-child");
    assert_eq!(parsed["rows"][1][0], "from-root");
    // And each row says which source it came through.
    assert_ne!(parsed["rows"][0][1], parsed["rows"][1][1]);
}

#[test]
fn a_query_over_a_broken_link_is_partial_and_still_answers() {
    let dir = project_with_links("federated-broken", "@link \"./absent.nostdb\"\n");
    let root = dir.path().to_string_lossy().into_owned();

    let result = nostdb([
        "query",
        "MATCH (n) RETURN n.name",
        "--format",
        "json",
        "--project",
        &root,
    ]);
    assert_eq!(result.class, ExitClass::Success, "{}", result.err);
    let parsed: serde_json::Value = serde_json::from_str(&result.out).expect("JSON");
    assert_eq!(parsed["summary"]["partial"], true, "{}", result.out);
    assert_eq!(
        parsed["summary"]["rows"], 1,
        "the reachable row is still returned"
    );
    assert_eq!(parsed["warnings"][0]["code"], "LINK_UNAVAILABLE");
}

#[test]
fn a_write_naming_a_linked_record_is_refused_and_the_target_is_untouched() {
    let target = TempDir::new("federated-readonly-target");
    let target_root = target.path().to_string_lossy().into_owned();
    assert_eq!(nostdb(["init", &target_root]).class, ExitClass::Success);
    assert_eq!(
        nostdb([
            "query",
            "CREATE (n:Linked {name: \"original\"})",
            "--project",
            &target_root
        ])
        .class,
        ExitClass::Success
    );

    let dir = TempDir::new("federated-readonly-root");
    let root = dir.path().to_string_lossy().into_owned();
    assert_eq!(nostdb(["init", &root]).class, ExitClass::Success);
    let source = dir.join("seed.nost");
    fs::write(
        &source,
        format!(
            "@nost 2\n\n@link \"{}\"\n\nnode local: Function {{}}\n",
            target.path().join(".nostdb/root.nostdb").display()
        ),
    )
    .unwrap();
    assert_eq!(
        nostdb([
            "convert",
            source.to_str().unwrap(),
            dir.join(".nostdb/root.nostdb").to_str().unwrap()
        ])
        .class,
        ExitClass::Success
    );

    let refused = nostdb([
        "query",
        "MATCH (n:Linked) SET n.name = \"changed\"",
        "--project",
        &root,
    ]);
    assert_eq!(refused.class, ExitClass::Validation);
    assert!(
        refused.err.contains("LINKED_DATABASE_READ_ONLY"),
        "{}",
        refused.err
    );

    // The linked database still says what it said.
    let unchanged = nostdb([
        "query",
        "MATCH (n:Linked) RETURN n.name",
        "--format",
        "json",
        "--project",
        &target_root,
    ]);
    let parsed: serde_json::Value = serde_json::from_str(&unchanged.out).unwrap();
    assert_eq!(parsed["rows"][0][0], "original");
}

// -- sync ------------------------------------------------------------------------

/// A configured project with materialization on and one node in the database.
fn materialized_project(label: &str) -> TempDir {
    let dir = TempDir::new(label);
    let root = dir.path().to_string_lossy().into_owned();
    assert_eq!(nostdb(["init", &root]).class, ExitClass::Success);
    fs::write(
        dir.join(".nostdb/settings.json"),
        "{\"settings_version\": 1, \"database\": {\"nost\": true}}",
    )
    .unwrap();
    assert_eq!(
        nostdb([
            "query",
            "CREATE (n:Function {name: \"seed\"})",
            "--project",
            &root
        ])
        .class,
        ExitClass::Success
    );
    dir
}

#[test]
fn sync_materializes_a_missing_file_then_reports_up_to_date() {
    let dir = materialized_project("sync-materialize");
    let root = dir.path().to_string_lossy().into_owned();

    let first = nostdb(["sync", &root]);
    assert_eq!(first.class, ExitClass::Success, "{}", first.err);
    assert!(dir.join(".nostdb/root.nost").is_file());
    assert!(
        dir.join(".nostdb/sync.json").is_file(),
        "a baseline is recorded"
    );

    let second = nostdb(["sync", &root]);
    assert_eq!(second.class, ExitClass::Success);
    assert!(second.out.contains("up to date"), "{}", second.out);
}

#[test]
fn sync_adopts_an_edited_document() {
    let dir = materialized_project("sync-adopt");
    let root = dir.path().to_string_lossy().into_owned();
    assert_eq!(nostdb(["sync", &root]).class, ExitClass::Success);

    let nost = dir.join(".nostdb/root.nost");
    let edited = format!(
        "{}\nnode added: Function {{\n  name: \"added\",\n}}\n",
        fs::read_to_string(&nost).unwrap()
    );
    fs::write(&nost, edited).unwrap();

    let synced = nostdb(["sync", &root]);
    assert_eq!(synced.class, ExitClass::Success, "{}", synced.err);
    assert!(synced.out.contains("adopted"), "{}", synced.out);

    let counted = nostdb([
        "query",
        "MATCH (n) RETURN n.name",
        "--format",
        "json",
        "--project",
        &root,
    ]);
    let parsed: serde_json::Value = serde_json::from_str(&counted.out).unwrap();
    assert_eq!(
        parsed["summary"]["rows"], 2,
        "the edit reached the database"
    );
}

#[test]
fn a_changed_database_leaves_the_document_stale_rather_than_regenerating_it() {
    let dir = materialized_project("sync-stale");
    let root = dir.path().to_string_lossy().into_owned();
    assert_eq!(nostdb(["sync", &root]).class, ExitClass::Success);
    let before = fs::read_to_string(dir.join(".nostdb/root.nost")).unwrap();

    assert_eq!(
        nostdb([
            "query",
            "CREATE (n:Function {name: \"only-in-database\"})",
            "--project",
            &root
        ])
        .class,
        ExitClass::Success
    );

    let synced = nostdb(["sync", &root]);
    assert_eq!(synced.class, ExitClass::Conflict);
    assert!(synced.err.contains("NOST_SOURCE_STALE"), "{}", synced.err);
    assert_eq!(
        fs::read_to_string(dir.join(".nostdb/root.nost")).unwrap(),
        before,
        "a stale file may hold edits its author has not applied"
    );
}

#[test]
fn both_sides_changing_is_a_conflict_that_modifies_neither() {
    let dir = materialized_project("sync-conflict");
    let root = dir.path().to_string_lossy().into_owned();
    assert_eq!(nostdb(["sync", &root]).class, ExitClass::Success);

    assert_eq!(
        nostdb([
            "query",
            "CREATE (n:Function {name: \"in-database\"})",
            "--project",
            &root
        ])
        .class,
        ExitClass::Success
    );
    let nost = dir.join(".nostdb/root.nost");
    let edited = format!(
        "{}\nnode added: Function {{\n  name: \"in-file\",\n}}\n",
        fs::read_to_string(&nost).unwrap()
    );
    fs::write(&nost, &edited).unwrap();
    let database_before = fs::read(dir.join(".nostdb/root.nostdb")).unwrap();

    let synced = nostdb(["sync", &root]);
    assert_eq!(synced.class, ExitClass::Conflict);
    assert!(synced.err.contains("SYNC_CONFLICT"), "{}", synced.err);
    assert!(synced.err.contains("human decision"), "{}", synced.err);

    assert_eq!(
        fs::read_to_string(&nost).unwrap(),
        edited,
        "the file is untouched"
    );
    assert_eq!(
        fs::read(dir.join(".nostdb/root.nostdb")).unwrap(),
        database_before,
        "and so is the database"
    );
}

#[test]
fn sync_declines_when_no_baseline_records_what_the_two_agreed_on() {
    let dir = materialized_project("sync-no-baseline");
    let root = dir.path().to_string_lossy().into_owned();
    assert_eq!(nostdb(["sync", &root]).class, ExitClass::Success);
    fs::remove_file(dir.join(".nostdb/sync.json")).unwrap();

    let synced = nostdb(["sync", &root]);
    assert_eq!(synced.class, ExitClass::Conflict);
    assert!(synced.err.contains("last agreed on"), "{}", synced.err);
    // And it says how to establish one, rather than only that it cannot proceed.
    assert!(synced.err.contains("export --nost"), "{}", synced.err);
    assert!(synced.err.contains("convert"), "{}", synced.err);
}

#[test]
fn a_refused_document_leaves_the_database_exactly_as_it_was() {
    let dir = materialized_project("sync-refused");
    let root = dir.path().to_string_lossy().into_owned();
    assert_eq!(nostdb(["sync", &root]).class, ExitClass::Success);
    let before = fs::read(dir.join(".nostdb/root.nostdb")).unwrap();

    fs::write(
        dir.join(".nostdb/root.nost"),
        "@nost 2\nnode a: L {\n  id: \"n_1\",\n}\n",
    )
    .unwrap();

    let synced = nostdb(["sync", &root]);
    assert_eq!(synced.class, ExitClass::Validation);
    assert!(synced.err.contains("NOST_INVALID_ID"), "{}", synced.err);
    assert_eq!(fs::read(dir.join(".nostdb/root.nostdb")).unwrap(), before);
}

#[test]
fn sync_says_there_is_nothing_to_compare_when_materialization_is_off() {
    let dir = TempDir::new("sync-off");
    let root = dir.path().to_string_lossy().into_owned();
    assert_eq!(nostdb(["init", &root]).class, ExitClass::Success);

    let synced = nostdb(["sync", &root]);
    assert_eq!(synced.class, ExitClass::Success, "{}", synced.err);
    assert!(synced.err.contains("nothing to compare"), "{}", synced.err);
    assert!(!dir.join(".nostdb/root.nost").exists());
}

#[test]
fn export_records_a_baseline_so_sync_can_proceed_afterwards() {
    let dir = materialized_project("sync-after-export");
    let root = dir.path().to_string_lossy().into_owned();

    assert_eq!(
        nostdb(["export", "--nost", &root]).class,
        ExitClass::Success
    );
    assert!(dir.join(".nostdb/sync.json").is_file());

    let synced = nostdb(["sync", &root]);
    assert_eq!(synced.class, ExitClass::Success, "{}", synced.err);
    assert!(synced.out.contains("up to date"), "{}", synced.out);
}

#[test]
fn sync_outside_a_project_is_a_usage_error() {
    let dir = TempDir::new("sync-unconfigured");
    let synced = nostdb(["sync", dir.path().to_str().unwrap()]);
    assert_eq!(synced.class, ExitClass::Usage);
    assert!(synced.err.contains("nostdb init"), "{}", synced.err);
}
