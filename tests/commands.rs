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
    nostdb_run(&arguments)
}

/// The same, for a case that builds its arguments rather than writing them out.
fn nostdb_run(arguments: &[&str]) -> Output {
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
fn refresh_reports_a_local_link_as_having_no_snapshot_rather_than_failing() {
    // The reason it was refused for two Stages, now answered rather than deferred: a local
    // link is read live at every query, so there is nothing to advance.
    let dir = TempDir::new("link-refresh-local");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    nostdb(["link", "add", "./child", "--project", &root]);

    let result = nostdb(["link", "refresh", "--format", "json", "--project", &root]);
    assert_eq!(result.class, ExitClass::Success, "{}", result.err);
    let document: serde_json::Value = serde_json::from_str(&result.out).unwrap();
    assert_eq!(document["links"][0]["outcome"], "not_remote");
}

#[test]
fn refresh_without_a_provider_reports_the_link_unavailable_rather_than_failing() {
    // An unreachable source keeps its declaration. A missing provider is the same kind of
    // fact about this machine as an unreachable host is about the network.
    let dir = TempDir::new("link-refresh-no-provider");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    nostdb([
        "link",
        "add",
        "github://example/payments/?ref=main",
        "--project",
        &root,
    ]);

    let result = nostdb(["link", "refresh", "--format", "json", "--project", &root]);
    assert_eq!(result.class, ExitClass::Success, "{}", result.err);
    let document: serde_json::Value = serde_json::from_str(&result.out).unwrap();
    assert_eq!(document["links"][0]["outcome"], "unavailable");
    assert!(result.err.contains("warning:"), "{}", result.err);
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

#[test]
fn link_add_declares_a_link_and_link_list_reports_it() {
    let dir = TempDir::new("link-add");
    let root = dir.path().to_string_lossy().into_owned();
    let target = dir.join("child");
    fs::create_dir_all(&target).unwrap();
    nostdb(["init", &root]);
    nostdb(["init", target.to_str().unwrap()]);

    let added = nostdb(["link", "add", "./child", "as", "child", "--project", &root]);
    assert_eq!(added.class, ExitClass::Success, "{}", added.err);
    assert!(added.out.contains("./child"), "{}", added.out);
    assert!(added.out.contains("as child"), "{}", added.out);

    let listed = nostdb(["link", "list", "--format", "json", "--project", &root]);
    assert_eq!(listed.class, ExitClass::Success, "{}", listed.err);
    let document: serde_json::Value = serde_json::from_str(&listed.out).unwrap();
    assert_eq!(document["links"][0]["source"], "./child");
    assert_eq!(document["links"][0]["alias"], "child");
    assert_eq!(document["links"][0]["available"], true);

    let checked = nostdb(["link", "check", "--project", &root]);
    assert_eq!(checked.class, ExitClass::Success, "{}", checked.err);
}

#[test]
fn link_add_does_not_require_the_target_to_be_reachable() {
    // Whether a source resolves is a separate question from whether it is declared. A
    // sibling that has not been cloned yet is exactly the case `check` exists to report.
    let dir = TempDir::new("link-add-unreachable");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);

    let added = nostdb(["link", "add", "./not-cloned-yet", "--project", &root]);
    assert_eq!(added.class, ExitClass::Success, "{}", added.err);

    let listed = nostdb(["link", "list", "--project", &root]);
    assert_eq!(
        listed.class,
        ExitClass::Success,
        "list reports; it does not judge"
    );
    let checked = nostdb(["link", "check", "--project", &root]);
    assert_eq!(checked.class, ExitClass::Unavailable, "{}", checked.err);
}

#[test]
fn link_add_writes_the_source_into_the_settings_and_the_alias_only_into_the_graph() {
    let dir = TempDir::new("link-add-mirror");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    nostdb(["link", "add", "./child", "as", "child", "--project", &root]);

    let settings = fs::read_to_string(dir.join(".nostdb/settings.json")).unwrap();
    let document: serde_json::Value = serde_json::from_str(&settings).unwrap();
    assert_eq!(document["links"][0]["source"], "./child");
    assert!(
        document["links"][0].get("alias").is_none(),
        "the settings contract rejects an entry carrying an alias: {settings}"
    );
}

#[test]
fn link_remove_removes_the_declaration_and_the_mirror() {
    let dir = TempDir::new("link-remove");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    nostdb(["link", "add", "./child", "--project", &root]);

    let removed = nostdb(["link", "remove", "./child", "--project", &root]);
    assert_eq!(removed.class, ExitClass::Success, "{}", removed.err);

    let listed = nostdb(["link", "list", "--format", "json", "--project", &root]);
    let document: serde_json::Value = serde_json::from_str(&listed.out).unwrap();
    assert_eq!(document["summary"]["declared"], 0);

    let settings = fs::read_to_string(dir.join(".nostdb/settings.json")).unwrap();
    let document: serde_json::Value = serde_json::from_str(&settings).unwrap();
    assert_eq!(document["links"].as_array().unwrap().len(), 0);
}

#[test]
fn a_refused_link_change_is_a_validation_failure_that_changed_nothing() {
    let dir = TempDir::new("link-refused");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    nostdb(["link", "add", "./child", "as", "child", "--project", &root]);
    let before = fs::read(dir.join(".nostdb/root.nostdb")).unwrap();

    for arguments in [
        vec!["link", "add", "./child", "--project", &root],
        vec!["link", "add", "./other", "as", "child", "--project", &root],
        vec!["link", "remove", "./nothing", "--project", &root],
    ] {
        let owned: Vec<String> = arguments.iter().map(|a| (*a).to_owned()).collect();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let class = run(&owned, &mut out, &mut err);
        assert_eq!(class, ExitClass::Validation, "{arguments:?}");
        assert!(out.is_empty(), "{arguments:?} wrote to stdout");
        assert!(!err.is_empty(), "{arguments:?} explained nothing");
    }

    assert_eq!(
        fs::read(dir.join(".nostdb/root.nostdb")).unwrap(),
        before,
        "a refused change preserves the last valid generation byte for byte"
    );
}

#[test]
fn a_link_change_refuses_rather_than_overwriting_a_hand_edited_nost() {
    let dir = TempDir::new("link-unsynchronized");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    fs::write(
        dir.join(".nostdb/settings.json"),
        "{\"settings_version\": 1, \"database\": {\"nost\": true}}",
    )
    .unwrap();
    assert_eq!(nostdb(["sync", &root]).class, ExitClass::Success);

    let nost = dir.join(".nostdb/root.nost");
    let edited = format!(
        "{}\n// written by hand\n",
        fs::read_to_string(&nost).unwrap()
    );
    fs::write(&nost, &edited).unwrap();

    let result = nostdb(["link", "add", "./child", "--project", &root]);
    assert_eq!(result.class, ExitClass::Conflict, "{}", result.err);
    assert!(result.out.is_empty(), "{}", result.out);
    assert!(result.err.contains("nostdb sync"), "{}", result.err);
    assert_eq!(
        fs::read_to_string(&nost).unwrap(),
        edited,
        "the hand-written line survives the refusal"
    );
}

#[test]
fn a_link_change_keeps_a_materialized_nost_current_and_in_agreement() {
    let dir = TempDir::new("link-materialized");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    fs::write(
        dir.join(".nostdb/settings.json"),
        "{\"settings_version\": 1, \"database\": {\"nost\": true}}",
    )
    .unwrap();
    assert_eq!(nostdb(["sync", &root]).class, ExitClass::Success);

    let added = nostdb(["link", "add", "./child", "as", "child", "--project", &root]);
    assert_eq!(added.class, ExitClass::Success, "{}", added.err);

    let nost = fs::read_to_string(dir.join(".nostdb/root.nost")).unwrap();
    assert!(nost.contains("@link \"./child\" as child"), "{nost}");

    let synced = nostdb(["sync", &root]);
    assert_eq!(
        synced.class,
        ExitClass::Success,
        "the change must leave the two agreeing: {}",
        synced.err
    );
    assert!(synced.out.contains("up to date"), "{}", synced.out);
}

#[test]
fn link_add_reports_json_when_asked_and_keeps_notes_off_stdout() {
    let dir = TempDir::new("link-json");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);

    let added = nostdb([
        "link",
        "add",
        "./child",
        "as",
        "child",
        "--format",
        "json",
        "--project",
        &root,
    ]);
    assert_eq!(added.class, ExitClass::Success, "{}", added.err);
    let document: serde_json::Value = serde_json::from_str(&added.out).unwrap();
    assert_eq!(document["action"], "added");
    assert_eq!(document["source"], "./child");
    assert_eq!(document["alias"], "child");
    assert_eq!(document["settings_updated"], true);
    assert_eq!(document["nost_updated"], false);
    assert!(document["database_generation"].as_u64().unwrap() >= 2);
}

#[test]
fn plan_reports_what_a_build_would_do_and_accounts_for_every_file() {
    let dir = TempDir::new("plan");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    fs::write(dir.join(".gitignore"), "*.log\n").unwrap();
    fs::write(dir.join("main.rs"), "fn main() {}\n").unwrap();
    fs::write(dir.join("app.py"), "def main(): pass\n").unwrap();
    fs::write(dir.join("debug.log"), "noise\n").unwrap();
    fs::write(dir.join(".env"), "SECRET=1\n").unwrap();

    let result = nostdb(["plan", "--format", "json", "--project", &root]);
    assert_eq!(result.class, ExitClass::Success, "{}", result.err);
    let document: serde_json::Value = serde_json::from_str(&result.out).unwrap();

    assert_eq!(document["plan_version"], 1);
    assert_eq!(document["scanned_files"], 2, "{}", result.out);
    assert_eq!(
        document["structural_files"], 1,
        "the Rust file is covered deterministically: {}",
        result.out
    );
    assert_eq!(
        document["unsupported_files"], 1,
        "the Python file is not, and stays eligible for AI instead"
    );
    assert_eq!(
        document["semantic_candidates"], 1,
        "a file a deterministic analyzer covers is not a candidate for enrichment"
    );
    assert_eq!(document["semantic_cache_hits"], 0);

    let languages: Vec<(&str, &str)> = document["languages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["language"].as_str().unwrap(),
                entry["precision"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        languages,
        [
            ("python", "unsupported"),
            ("rust", "deterministic syntactic")
        ],
        "precision travels with every language, so nobody can read a syntactic fact as a \
         resolved one"
    );

    let reasons: Vec<&str> = document["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["reason"].as_str().unwrap())
        .collect();
    assert!(reasons.contains(&"ignored"), "{reasons:?}");
    assert!(reasons.contains(&"sensitive"), "{reasons:?}");
}

#[test]
fn plan_never_mistakes_a_local_tree_for_a_commit() {
    let dir = TempDir::new("plan-revision");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    fs::write(dir.join("main.rs"), "fn main() {}\n").unwrap();

    let first = nostdb(["plan", "--format", "json", "--project", &root]);
    let first: serde_json::Value = serde_json::from_str(&first.out).unwrap();
    let revision = first["source_revision"].as_str().unwrap().to_owned();
    assert!(revision.starts_with("tree:"), "{revision}");

    // Unchanged source, unchanged revision. That is what an incremental rebuild reads.
    let again = nostdb(["plan", "--format", "json", "--project", &root]);
    let again: serde_json::Value = serde_json::from_str(&again.out).unwrap();
    assert_eq!(again["source_revision"], revision.as_str());

    fs::write(dir.join("main.rs"), "fn main() { let x = 1; }\n").unwrap();
    let changed = nostdb(["plan", "--format", "json", "--project", &root]);
    let changed: serde_json::Value = serde_json::from_str(&changed.out).unwrap();
    assert_ne!(changed["source_revision"], revision.as_str());
}

#[test]
fn plan_with_ai_off_spends_nothing_and_says_so_on_stderr() {
    let dir = TempDir::new("plan-ai-off");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    fs::write(
        dir.join(".nostdb/settings.json"),
        "{\"settings_version\": 1, \"analysis\": {\"ai_mode\": \"off\"}}",
    )
    .unwrap();
    fs::write(dir.join("app.py"), "def main(): pass\n").unwrap();

    let result = nostdb(["plan", "--format", "json", "--project", &root]);
    assert_eq!(result.class, ExitClass::Success, "{}", result.err);
    let document: serde_json::Value = serde_json::from_str(&result.out).unwrap();
    assert_eq!(
        document["semantic_candidates"], 1,
        "what could be enriched is a fact about the source"
    );
    assert_eq!(document["estimated_input_tokens"]["high"], 0);
    assert!(result.err.contains("ai_mode is off"), "{}", result.err);
}

#[test]
fn plan_exits_eight_when_the_estimate_would_cross_a_configured_limit() {
    // Planning succeeded. It is the plan that says the build cannot proceed, and it says
    // so before anything is spent, which is the whole reason the command exists.
    let dir = TempDir::new("plan-over-budget");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    fs::write(
        dir.join(".nostdb/settings.json"),
        "{\"settings_version\": 1, \"analysis\": {\"max_input_tokens\": 10}}",
    )
    .unwrap();
    fs::write(dir.join("app.py"), "def main(): pass\n".repeat(200)).unwrap();

    let result = nostdb(["plan", "--project", &root]);
    assert_eq!(result.class, ExitClass::AiBudget, "{}", result.err);
    assert!(
        result.out.contains("tokens"),
        "the plan is still reported: {}",
        result.out
    );
    assert!(result.err.contains("max_input_tokens"), "{}", result.err);
}

#[test]
fn plan_writes_the_report_to_stdout_and_every_note_to_stderr() {
    let dir = TempDir::new("plan-streams");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    fs::write(dir.join("app.py"), "def main(): pass\n").unwrap();

    let result = nostdb(["plan", "--project", &root]);
    assert_eq!(result.class, ExitClass::Success, "{}", result.err);
    assert!(result.out.contains("revision"), "{}", result.out);
    assert!(result.out.contains("python"), "{}", result.out);
    // No limit is configured, so the contract requires asking before enrichment. The note
    // saying so is commentary and must not reach the data stream.
    assert!(result.err.contains("no token limit"), "{}", result.err);
    assert!(!result.out.contains("note:"), "{}", result.out);
}

#[test]
fn plan_reports_structural_coverage_once_an_analyzer_reads_the_language() {
    // This is the whole pipeline joined up for the first time: the scanner names the
    // language, the registry says an analyzer covers it, and the plan reports coverage
    // rather than a gap.
    let dir = TempDir::new("plan-covered");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    fs::write(dir.join("main.rs"), "fn main() {}\n").unwrap();

    let result = nostdb(["plan", "--project", &root]);
    assert_eq!(result.class, ExitClass::Success, "{}", result.err);
    assert!(
        result.out.contains("1 covered, 0 unsupported"),
        "{}",
        result.out
    );
    assert!(
        result.out.contains("deterministic syntactic"),
        "the precision is shown, not just the count: {}",
        result.out
    );
    assert!(
        !result.err.contains("no deterministic analyzer"),
        "the build has one now: {}",
        result.err
    );
    assert!(
        !result.err.contains("no token limit"),
        "nothing is a candidate for enrichment, so nothing would be asked about: {}",
        result.err
    );
}

#[test]
fn plan_outside_a_project_is_a_usage_mistake() {
    let dir = TempDir::new("plan-unconfigured");
    let root = dir.path().to_string_lossy().into_owned();
    let result = nostdb(["plan", "--project", &root]);
    assert_eq!(result.class, ExitClass::Usage);
    assert!(result.out.is_empty(), "{}", result.out);
    assert!(result.err.contains("nostdb init"), "{}", result.err);
}

#[test]
fn build_commits_the_facts_and_a_query_can_then_find_them() {
    // The pipeline end to end: scan, analyze, commit, query. Nothing in this test knows
    // how any of those work, which is the point.
    let dir = TempDir::new("build");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("src/main.rs"),
        "fn main() { helper(); }\nfn helper() {}\nstruct Config { port: u32 }\n",
    )
    .unwrap();

    let built = nostdb(["build", "--format", "json", "--project", &root]);
    assert_eq!(built.class, ExitClass::Success, "{}", built.err);
    let document: serde_json::Value = serde_json::from_str(&built.out).unwrap();
    assert_eq!(document["analyzed_files"], 1);
    assert_eq!(document["references"]["resolved"], 1);
    assert_eq!(document["coverage"]["structural"], "complete");
    assert_eq!(
        document["coverage"]["semantic"], "skipped",
        "a structural build runs no AI, and says so rather than implying it"
    );
    assert!(document["records"]["nodes_created"].as_u64().unwrap() >= 4);

    let queried = nostdb([
        "query",
        "MATCH (f:Function) RETURN f.name ORDER BY f.name",
        "--format",
        "json",
        "--project",
        &root,
    ]);
    assert_eq!(queried.class, ExitClass::Success, "{}", queried.err);
    let result: serde_json::Value = serde_json::from_str(&queried.out).unwrap();
    assert_eq!(result["rows"][0][0], "helper");
    assert_eq!(result["rows"][1][0], "main");

    let calls = nostdb([
        "query",
        "MATCH (a:Function)-[:CALLS]->(b:Function) RETURN a.name, b.name",
        "--format",
        "json",
        "--project",
        &root,
    ]);
    let result: serde_json::Value = serde_json::from_str(&calls.out).unwrap();
    assert_eq!(result["rows"][0][0], "main");
    assert_eq!(result["rows"][0][1], "helper");
}

#[test]
fn a_rebuild_removes_what_the_source_no_longer_declares() {
    let dir = TempDir::new("build-rebuild");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    fs::write(dir.join("lib.rs"), "fn kept() {}\nfn removed() {}\n").unwrap();
    nostdb(["build", "--project", &root]);

    fs::write(dir.join("lib.rs"), "fn kept() {}\n").unwrap();
    let rebuilt = nostdb(["build", "--format", "json", "--project", &root]);
    assert_eq!(rebuilt.class, ExitClass::Success, "{}", rebuilt.err);

    let queried = nostdb([
        "query",
        "MATCH (f:Function) RETURN f.name",
        "--format",
        "json",
        "--project",
        &root,
    ]);
    let result: serde_json::Value = serde_json::from_str(&queried.out).unwrap();
    assert_eq!(result["summary"]["rows"], 1);
    assert_eq!(result["rows"][0][0], "kept");
}

#[test]
fn a_project_with_nothing_to_analyze_succeeds_and_says_so_on_stderr() {
    // Exiting non-zero would break a pipeline that runs `build` before knowing what a
    // repository contains.
    let dir = TempDir::new("build-nothing");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    fs::write(dir.join("notes.txt"), "nothing here\n").unwrap();

    let built = nostdb(["build", "--project", &root]);
    assert_eq!(built.class, ExitClass::Success, "{}", built.err);
    assert!(
        built.err.contains("no file has a language"),
        "{}",
        built.err
    );
    assert!(built.out.contains("0 files"), "{}", built.out);
}

#[test]
fn build_outside_a_project_is_a_usage_mistake_and_writes_nothing_to_stdout() {
    let dir = TempDir::new("build-unconfigured");
    let root = dir.path().to_string_lossy().into_owned();
    let built = nostdb(["build", "--project", &root]);
    assert_eq!(built.class, ExitClass::Usage);
    assert!(built.out.is_empty(), "{}", built.out);
    assert!(built.err.contains("nostdb init"), "{}", built.err);
}

#[test]
fn a_build_that_analyzed_nothing_names_the_languages_on_both_sides() {
    // Reported: a Kotlin repository built to `0 nodes, 0 edges` and was read as a build failure.
    // It was not — this build ships one analyzer — but the note said only that no file had a
    // language it analyzes, which leaves a reader to work out whether they excluded their own
    // sources by mistake or whether the language has no analyzer yet. Those have opposite fixes,
    // so the note has to name what it analyzes and what it found.
    let dir = TempDir::new("build-unsupported-language");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    fs::write(dir.join("Server.kt"), "class Server(val port: Int)\n").unwrap();

    let built = nostdb(["build", "--format", "json", "--project", &root]);
    // Not a failure: a project with nothing this build reads is a fact about the project.
    assert_eq!(built.class, ExitClass::Success, "{}", built.err);
    let document: serde_json::Value = serde_json::from_str(&built.out).unwrap();
    assert_eq!(document["analyzed_files"], 0);
    assert_eq!(document["records"]["nodes_created"], 0);

    assert!(
        built.err.contains("it analyzes rust"),
        "the note has to name what this build does analyze: {}",
        built.err
    );
    assert!(
        built.err.contains("this project is kotlin"),
        "and what it found, which is the half that says why the count is zero: {}",
        built.err
    );
}

#[test]
fn a_build_and_a_plan_agree_about_which_files_they_cover() {
    // They share one registry. A language `plan` calls unsupported and `build` analyzes
    // would make the two disagree about the same file.
    let dir = TempDir::new("build-plan-agree");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
    fs::write(dir.join("b.py"), "def b(): pass\n").unwrap();

    let planned = nostdb(["plan", "--format", "json", "--project", &root]);
    let planned: serde_json::Value = serde_json::from_str(&planned.out).unwrap();
    let built = nostdb(["build", "--format", "json", "--project", &root]);
    let built: serde_json::Value = serde_json::from_str(&built.out).unwrap();

    assert_eq!(planned["structural_files"], built["analyzed_files"]);
    assert_eq!(
        planned["source_revision"], built["source_revision"],
        "the same tree is the same snapshot to both"
    );
}

#[test]
fn a_second_build_reuses_everything_and_says_so() {
    let dir = TempDir::new("build-reuse");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    fs::write(dir.join("lib.rs"), "fn only() {}\n").unwrap();
    nostdb(["build", "--project", &root]);

    let again = nostdb(["build", "--format", "json", "--project", &root]);
    assert_eq!(again.class, ExitClass::Success, "{}", again.err);
    let document: serde_json::Value = serde_json::from_str(&again.out).unwrap();
    assert_eq!(document["analyzed_files"], 0);
    assert_eq!(document["reused_files"], 1);
    assert!(
        again.err.contains("matched the digest already recorded"),
        "{}",
        again.err
    );
}

#[test]
fn rebuild_re_reads_what_reuse_would_have_skipped() {
    let dir = TempDir::new("build-forced");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    fs::write(dir.join("lib.rs"), "fn only() {}\n").unwrap();
    nostdb(["build", "--project", &root]);

    let forced = nostdb(["build", "--rebuild", "--format", "json", "--project", &root]);
    assert_eq!(forced.class, ExitClass::Success, "{}", forced.err);
    let document: serde_json::Value = serde_json::from_str(&forced.out).unwrap();
    assert_eq!(document["analyzed_files"], 1);
    assert_eq!(document["reused_files"], 0);
}

#[test]
fn a_deleted_file_takes_its_records_out_of_the_database() {
    let dir = TempDir::new("build-deleted");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    fs::write(dir.join("kept.rs"), "fn kept() {}\n").unwrap();
    fs::write(dir.join("gone.rs"), "fn departed() {}\n").unwrap();
    nostdb(["build", "--project", &root]);

    fs::remove_file(dir.join("gone.rs")).unwrap();
    let rebuilt = nostdb(["build", "--project", &root]);
    assert_eq!(rebuilt.class, ExitClass::Success, "{}", rebuilt.err);

    let queried = nostdb([
        "query",
        "MATCH (f:Function) RETURN f.name",
        "--format",
        "json",
        "--project",
        &root,
    ]);
    let result: serde_json::Value = serde_json::from_str(&queried.out).unwrap();
    assert_eq!(result["summary"]["rows"], 1);
    assert_eq!(result["rows"][0][0], "kept");
}

fn change_set(generation: u64, operations: &str) -> String {
    format!(
        r#"{{
  "change_set_version": 1,
  "base_generation": {generation},
  "owner": {{"kind": "user"}},
  "source_snapshot": "by hand",
  "operations": [{operations}]
}}"#
    )
}

#[test]
fn apply_commits_a_hand_written_change_set_and_a_query_finds_it() {
    let dir = TempDir::new("apply");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    let file = dir.join("change.json");
    fs::write(
        &file,
        change_set(
            1,
            r#"{"operation": "upsert_node", "labels": ["Note"],
                "properties": {"text": "written by hand"},
                "source_unit": "u_00000000-0000-0000-0000-000000000000", "evidence": []}"#,
        ),
    )
    .unwrap();

    let applied = nostdb([
        "apply",
        file.to_str().unwrap(),
        "--format",
        "json",
        "--project",
        &root,
    ]);
    assert_eq!(applied.class, ExitClass::Success, "{}", applied.err);
    let document: serde_json::Value = serde_json::from_str(&applied.out).unwrap();
    assert_eq!(document["records"]["nodes_created"], 1);
    assert_eq!(document["operations"], 1);

    let queried = nostdb([
        "query",
        "MATCH (n:Note) RETURN n.text",
        "--format",
        "json",
        "--project",
        &root,
    ]);
    let result: serde_json::Value = serde_json::from_str(&queried.out).unwrap();
    assert_eq!(result["rows"][0][0], "written by hand");
}

#[test]
fn a_malformed_change_set_reports_every_problem_at_once() {
    // One failed run per mistake would make fixing a batch a loop.
    let dir = TempDir::new("apply-malformed");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    let file = dir.join("broken.json");
    fs::write(
        &file,
        change_set(
            1,
            r#"{"operation": "upsert_node", "labels": [],
                "source_unit": "u_00000000-0000-0000-0000-000000000000", "evidence": []},
               {"operation": "frobnicate"}"#,
        ),
    )
    .unwrap();

    let result = nostdb(["apply", file.to_str().unwrap(), "--project", &root]);
    assert_eq!(result.class, ExitClass::Validation, "{}", result.err);
    assert!(result.out.is_empty(), "{}", result.out);
    assert!(result.err.contains("CHANGE_SET_INVALID"), "{}", result.err);
    assert!(
        result.err.lines().count() >= 2,
        "every problem, not the first: {}",
        result.err
    );
}

#[test]
fn a_change_set_computed_against_another_generation_is_refused_and_changes_nothing() {
    // It resolved identifiers against a graph it read. Applying it to a graph that has
    // moved would overwrite work nobody saw.
    let dir = TempDir::new("apply-stale");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    let before = fs::read(dir.join(".nostdb/root.nostdb")).unwrap();

    let file = dir.join("stale.json");
    fs::write(
        &file,
        change_set(
            99,
            r#"{"operation": "upsert_node", "labels": ["Note"],
                "source_unit": "u_00000000-0000-0000-0000-000000000000", "evidence": []}"#,
        ),
    )
    .unwrap();

    let result = nostdb(["apply", file.to_str().unwrap(), "--project", &root]);
    assert_ne!(result.class, ExitClass::Success);
    assert!(result.err.contains("generation"), "{}", result.err);
    assert_eq!(
        fs::read(dir.join(".nostdb/root.nostdb")).unwrap(),
        before,
        "a refused apply preserves the last valid generation byte for byte"
    );
}

#[test]
fn an_unreadable_change_set_version_says_so_rather_than_naming_an_operation() {
    let dir = TempDir::new("apply-version");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    let file = dir.join("future.json");
    fs::write(&file, r#"{"change_set_version": 99, "operations": []}"#).unwrap();

    let result = nostdb(["apply", file.to_str().unwrap(), "--project", &root]);
    assert_eq!(result.class, ExitClass::Validation);
    assert!(
        result.err.contains("CHANGE_SET_VERSION_UNSUPPORTED"),
        "{}",
        result.err
    );
    assert!(
        !result.err.contains("CHANGE_SET_INVALID"),
        "naming a malformed operation would send somebody looking for one: {}",
        result.err
    );
}

#[test]
fn apply_without_a_file_is_a_usage_mistake() {
    let dir = TempDir::new("apply-no-file");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    let result = nostdb(["apply", "--project", &root]);
    assert_eq!(result.class, ExitClass::Usage);
    assert!(result.out.is_empty());
}

#[test]
fn a_missing_change_set_file_is_an_io_failure() {
    let dir = TempDir::new("apply-absent");
    let root = dir.path().to_string_lossy().into_owned();
    nostdb(["init", &root]);
    let absent = dir.join("not-here.json");
    let result = nostdb(["apply", absent.to_str().unwrap(), "--project", &root]);
    assert_eq!(result.class, ExitClass::Io);
    assert!(result.out.is_empty());
}

/// `catalog add`, `list`, and `remove` against a catalog this test owns.
///
/// The catalog location is per operating-system user and fixed, so this drives the module directly
/// rather than through `run`, which would write the real `~/.nostdb/catalog.json`. A test that
/// edited a developer's own catalog would be a test with a side effect nobody asked for.
#[test]
fn the_catalog_commands_register_list_and_remove_a_name() {
    use nostdb_cli::catalog::{Action, execute, parse};
    use nostdb_cli::exit::ExitClass;
    use nostdb_cli::output::Format;

    let directory = std::env::temp_dir().join(format!("nostdb-cli-catalog-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("scratch");
    let catalog_path = directory.join("catalog.json");
    let _ = std::fs::remove_file(&catalog_path);
    let database = directory.join("root.nostdb");

    let mut out = Vec::new();
    let mut err = Vec::new();

    // An absent catalog lists nothing rather than failing.
    let class = execute(
        &Action::List {
            format: Format::Table,
        },
        &catalog_path,
        &mut out,
        &mut err,
    );
    assert_eq!(class, ExitClass::Success);
    assert!(out.is_empty(), "an empty catalog lists nothing");

    let add = parse(&["add", "work", database.to_str().expect("utf-8")]).expect("parsed");
    out.clear();
    assert_eq!(
        execute(&add, &catalog_path, &mut out, &mut err),
        ExitClass::Success
    );

    out.clear();
    execute(
        &Action::List {
            format: Format::Json,
        },
        &catalog_path,
        &mut out,
        &mut err,
    );
    let listed: serde_json::Value =
        serde_json::from_slice(&out).expect("json output is a JSON document");
    assert_eq!(listed["databases"][0]["name"], "work");
    assert_eq!(
        listed["databases"][0]["path"],
        database.display().to_string(),
        "the stored path must be the absolute one"
    );

    // Removing a name that is not there is a validation failure, not a silent success.
    out.clear();
    err.clear();
    assert_eq!(
        execute(
            &Action::Remove {
                name: "absent".to_owned()
            },
            &catalog_path,
            &mut out,
            &mut err
        ),
        ExitClass::Validation
    );

    out.clear();
    assert_eq!(
        execute(
            &Action::Remove {
                name: "work".to_owned()
            },
            &catalog_path,
            &mut out,
            &mut err
        ),
        ExitClass::Success
    );

    out.clear();
    execute(
        &Action::List {
            format: Format::Table,
        },
        &catalog_path,
        &mut out,
        &mut err,
    );
    assert!(out.is_empty(), "the name was removed");
}

/// A name the catalog contract refuses is refused here, with class 3 rather than a panic.
#[test]
fn the_catalog_refuses_a_name_that_breaks_the_contract() {
    use nostdb_cli::catalog::{Action, execute};
    use nostdb_cli::exit::ExitClass;
    use std::path::PathBuf;

    let directory =
        std::env::temp_dir().join(format!("nostdb-cli-catalog-bad-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("scratch");
    let catalog_path = directory.join("catalog.json");
    let _ = std::fs::remove_file(&catalog_path);

    let mut out = Vec::new();
    let mut err = Vec::new();
    let class = execute(
        &Action::Add {
            name: "work/main".to_owned(),
            path: PathBuf::from("/srv/db.nostdb"),
        },
        &catalog_path,
        &mut out,
        &mut err,
    );
    assert_eq!(class, ExitClass::Validation);
    assert!(
        String::from_utf8_lossy(&err).contains("work/main"),
        "the refusal must name what was refused"
    );
    assert!(
        !catalog_path.exists(),
        "a refused add must not create a catalog"
    );
}

/// A whole round trip: start a daemon, register a name, query it, stop the daemon.
///
/// This is the only test that exercises the daemon route end to end from the command surface, and
/// it is the one that proves `--database @name` reaches a real database rather than that the
/// argument parses.
///
/// It is skipped unless `NOSTDB_DAEMON_TEST` is set, because it binds this user's real endpoint at
/// `~/.nostdb/run/nostdb.sock` and would fight a daemon a developer is already running. The
/// workspace verifier sets it.
#[test]
fn a_named_database_is_queried_through_the_daemon() {
    if std::env::var_os("NOSTDB_DAEMON_TEST").is_none() {
        println!("daemon round trip: skipped, NOSTDB_DAEMON_TEST is unset");
        return;
    }

    // This binds the user's real endpoint, and it stops the daemon when it is done. Stopping one
    // this test did not start would kill a daemon a developer is using, so it declines instead.
    if nostdb_server::is_running().unwrap_or(false) {
        println!(
            "daemon round trip: skipped, a daemon is already running and this test would stop it"
        );
        return;
    }

    use nostdb_cli::client::Client;
    use nostdb_cli::exit::ExitClass;

    let directory = std::env::temp_dir().join(format!("nostdb-cli-daemon-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("scratch");
    let database = directory.join("root.nostdb");
    let _ = std::fs::remove_file(&database);
    nostdb_core::storage::Database::create(&database).expect("created");

    // Register the name in this user's real catalog, and take it out again at the end.
    let catalog_path = nostdb_server::catalog::Catalog::default_path().expect("catalog path");
    let name = format!("clitest{}", std::process::id());
    let mut catalog = nostdb_server::catalog::Catalog::load(&catalog_path).expect("loaded");
    catalog.insert(&name, &database).expect("registered");
    catalog.store(&catalog_path).expect("stored");

    let mut out = Vec::new();
    let mut err = Vec::new();

    let started = nostdb_cli::client::start_daemon(
        std::path::Path::new(env!("CARGO_BIN_EXE_nostdb")),
        std::time::Duration::from_secs(10),
        &mut err,
    );
    assert!(
        started.is_ok(),
        "{started:?} {}",
        String::from_utf8_lossy(&err)
    );

    let class = nostdb_cli::query::named(
        &name,
        "MATCH (n) RETURN count(n) AS total",
        nostdb_cli::output::Format::Json,
        &mut out,
        &mut err,
    );
    assert_eq!(
        class,
        ExitClass::Success,
        "{}",
        String::from_utf8_lossy(&err)
    );

    let envelope: serde_json::Value =
        serde_json::from_slice(&out).expect("the daemon route emits the envelope as JSON");
    assert!(
        envelope["result_version"].is_number(),
        "the Engine's envelope must arrive intact: {envelope}"
    );
    assert_eq!(envelope["columns"], serde_json::json!(["total"]));

    // Stopping is part of the round trip: a test that left a daemon running would poison the next.
    let mut client = Client::connect().expect("connected");
    client.shutdown().expect("stopped");

    let mut catalog = nostdb_server::catalog::Catalog::load(&catalog_path).expect("loaded");
    catalog.remove(&name);
    catalog.store(&catalog_path).expect("cleaned up");
}

// `nostdb plugin`. The install itself is driven over a scripted provider in
// tests/plugin_install_flow.rs, because a provider conversation is what an install is made of.
// What these cases own is the surface: the grammar, the refusals a user meets before anything is
// fetched, and the two actions this build does not implement.

#[test]
fn plugin_needs_an_action_and_add_needs_a_source() {
    for arguments in [vec!["plugin"], vec!["plugin", "add"]] {
        let result = nostdb_run(&arguments);
        assert_eq!(result.class, ExitClass::Usage, "{arguments:?}");
        assert!(result.out.is_empty(), "{arguments:?} wrote to stdout");
        assert!(
            result.err.contains("plugin"),
            "{arguments:?}: {}",
            result.err
        );
    }
}

#[test]
fn plugin_list_reports_nothing_on_stderr_when_nothing_is_installed() {
    let temporary = TempDir::new("plugin-list-empty");
    let result = nostdb_run(&[
        "plugin",
        "list",
        "--scope",
        "project",
        "--project",
        &temporary.path().display().to_string(),
    ]);
    assert_eq!(result.class, ExitClass::Success);
    // An empty listing has no data, so a caller piping this receives nothing rather than a
    // sentence. The explanation goes to stderr.
    assert!(result.out.is_empty(), "{}", result.out);
    assert!(
        result.err.contains("nothing is installed"),
        "{}",
        result.err
    );
}

#[test]
fn removing_a_plugin_that_is_not_installed_reports_that_one_is_required() {
    let temporary = TempDir::new("plugin-remove-absent");
    let result = nostdb_run(&[
        "plugin",
        "remove",
        "org.example.absent",
        "--scope",
        "project",
        "--project",
        &temporary.path().display().to_string(),
    ]);
    assert_eq!(result.class, ExitClass::Plugin);
    assert!(result.err.contains("PLUGIN_REQUIRED"), "{}", result.err);
    assert!(result.err.contains("plugin list"), "{}", result.err);
}

#[test]
fn running_a_plugin_that_is_not_installed_reports_that_one_is_required() {
    let temporary = TempDir::new("plugin-run-absent");
    let result = nostdb_run(&[
        "plugin",
        "run",
        "org.example.absent",
        "view",
        "--scope",
        "project",
        "--project",
        &temporary.path().display().to_string(),
    ]);
    assert_eq!(result.class, ExitClass::Plugin);
    assert!(result.err.contains("PLUGIN_REQUIRED"), "{}", result.err);
    // The message names what would help. PLUGIN_REQUIRED is the code a caller branches on, and
    // this is what a person reads.
    assert!(result.err.contains("plugin add"), "{}", result.err);
}

#[test]
fn an_unknown_plugin_action_is_reported_as_unknown() {
    let result = nostdb_run(&["plugin", "frobnicate"]);
    assert_eq!(result.class, ExitClass::Usage);
    assert!(result.err.contains("frobnicate"), "{}", result.err);
}

#[test]
fn a_plugin_scope_must_be_project_or_global() {
    let result = nostdb_run(&[
        "plugin",
        "add",
        "https://github.com/o/r?ref=v1",
        "--scope",
        "user",
    ]);
    assert_eq!(result.class, ExitClass::Usage);
    assert!(result.err.contains("project or global"), "{}", result.err);
}

#[test]
fn a_plugin_source_that_is_not_github_is_refused_before_anything_is_fetched() {
    let result = nostdb_run(&["plugin", "add", "https://gitlab.com/o/r"]);
    assert_eq!(result.class, ExitClass::Validation);
    assert!(
        result.err.contains("PLUGIN_SOURCE_INVALID") && result.err.contains("GitHub"),
        "{}",
        result.err
    );
}

#[test]
fn a_plugin_source_carrying_a_credential_is_refused_rather_than_stripped() {
    let result = nostdb_run(&["plugin", "add", "https://github.com/token@o/r?ref=v1"]);
    assert_eq!(result.class, ExitClass::Validation);
    assert!(result.err.contains("credential"), "{}", result.err);
    // And the refusal never echoes what was passed, because a diagnostic is a place a secret
    // must not reach.
    assert!(!result.err.contains("token@"), "{}", result.err);
}

#[test]
fn a_plugin_source_with_no_ref_is_refused_and_says_what_to_do() {
    // A ref is required by the source grammar, so this is refused while parsing rather than on
    // the way to a provider. The first revision of the manifest contract said a manager resolves
    // a default branch with no ref, which the provider protocol forbade a locator from
    // expressing; the contract now requires the ref, and this is what that looks like to a user.
    let result = nostdb_run(&["plugin", "add", "https://github.com/o/r"]);
    assert_eq!(result.class, ExitClass::Validation);
    assert!(
        result.err.contains("PLUGIN_SOURCE_INVALID") && result.err.contains("ref=<git-ref>"),
        "{}",
        result.err
    );
}

#[test]
fn the_plugin_help_topic_states_that_a_plugin_is_not_sandboxed() {
    let result = nostdb_run(&["help", "plugin"]);
    assert_eq!(result.class, ExitClass::Success);
    // The root contract forbids claiming a sandbox that is not implemented. The positive form of
    // that check is requiring the disclaimer, which is the lesson increment 3 recorded.
    assert!(result.out.contains("not sandboxed"), "{}", result.out);
    assert!(
        result.out.contains("never executes plugin code"),
        "{}",
        result.out
    );
}

/// `build` and `plan` accept the positional path their own help advertises.
///
/// They did not. `help` published `build [PATH]` and `plan [PATH]` while the parser answered
/// "`build` does not take `.`", so the surface contradicted its own documentation — and contradicted
/// `init`, `sync`, and `export`, which all took one. Nothing caught it because every test passed the
/// path with `--project`, which is the spelling the parser did accept.
#[test]
fn build_and_plan_take_the_positional_path_their_help_publishes() {
    use nostdb_cli::Invocation;
    use std::path::PathBuf;

    for command in ["build", "plan"] {
        let arguments = vec![command.to_owned(), "./somewhere".to_owned()];
        let parsed = Invocation::parse(&arguments)
            .unwrap_or_else(|error| panic!("{command} ./somewhere: {error}"));
        let from = match parsed {
            Invocation::Build { from, .. } | Invocation::Plan { from, .. } => from,
            other => panic!("{command} parsed as {other:?}"),
        };
        assert_eq!(from, PathBuf::from("./somewhere"), "{command}");
    }
}

/// The same path given twice is refused rather than one of the two being picked.
#[test]
fn a_path_given_positionally_and_with_project_is_refused() {
    use nostdb_cli::Invocation;

    let arguments = vec![
        "build".to_owned(),
        "./one".to_owned(),
        "--project".to_owned(),
        "./two".to_owned(),
    ];
    let error = Invocation::parse(&arguments).expect_err("refused");
    let message = format!("{error}");
    assert!(
        message.contains("twice"),
        "the refusal must say what was wrong: {message}"
    );
}

/// `--rebuild` may appear on either side of the path.
#[test]
fn rebuild_and_the_positional_path_do_not_depend_on_order() {
    use nostdb_cli::Invocation;
    use std::path::PathBuf;

    for arguments in [
        vec!["build".to_owned(), "--rebuild".to_owned(), "./p".to_owned()],
        vec!["build".to_owned(), "./p".to_owned(), "--rebuild".to_owned()],
    ] {
        match Invocation::parse(&arguments).expect("parsed") {
            Invocation::Build {
                from,
                rebuild: true,
                ..
            } => assert_eq!(from, PathBuf::from("./p")),
            other => panic!("{arguments:?} parsed as {other:?}"),
        }
    }
}
