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
