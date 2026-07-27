//! The REPL, driven by feeding it a script.
//!
//! `repl` reads from a `BufRead` rather than the terminal, so a whole session runs
//! in-process and both streams are asserted alongside the exit class.

use nostdb_cli::output::Format;
use nostdb_cli::{ExitClass, query, run};
use std::fs;
use std::path::{Path, PathBuf};

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let mut base = std::env::temp_dir();
        base.push(format!("nostdb-cli-repl-{label}"));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("temporary directory");
        Self(base)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Session {
    class: ExitClass,
    out: String,
    err: String,
}

fn project(label: &str) -> TempDir {
    let dir = TempDir::new(label);
    let root = dir.path().to_string_lossy().into_owned();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let class = run(&["init".to_owned(), root], &mut out, &mut err);
    assert_eq!(
        class,
        ExitClass::Success,
        "{}",
        String::from_utf8_lossy(&err)
    );
    dir
}

fn session(dir: &TempDir, script: &str) -> Session {
    let mut input = std::io::BufReader::new(script.as_bytes());
    let mut out = Vec::new();
    let mut err = Vec::new();
    let class = query::repl(dir.path(), Format::Json, &mut input, &mut out, &mut err);
    Session {
        class,
        out: String::from_utf8(out).expect("stdout is UTF-8"),
        err: String::from_utf8(err).expect("stderr is UTF-8"),
    }
}

/// Every JSON document the session wrote, ignoring prompts and banners.
fn documents(out: &str) -> Vec<serde_json::Value> {
    let mut found = Vec::new();
    let mut depth = 0_usize;
    let mut current = String::new();
    for line in out.lines() {
        // Prompts share a line with output, and a multiline statement leaves a run of
        // continuation prompts ahead of the first `{`. Strip every prompt before deciding
        // where a document begins.
        let mut line = line;
        loop {
            let stripped = line
                .strip_prefix("nostdb> ")
                .or_else(|| line.strip_prefix("nostdb*> "))
                .or_else(|| line.strip_prefix("... "));
            match stripped {
                Some(rest) => line = rest,
                None => break,
            }
        }
        if depth == 0 && !line.starts_with('{') {
            continue;
        }
        depth += line.matches('{').count();
        depth = depth.saturating_sub(line.matches('}').count());
        current.push_str(line);
        current.push('\n');
        if depth == 0 {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&current) {
                found.push(value);
            }
            current.clear();
        }
    }
    found
}

#[test]
fn a_session_greets_names_its_database_and_leaves_cleanly() {
    let dir = project("greeting");
    let result = session(&dir, ":database\n:quit\n");
    assert_eq!(result.class, ExitClass::Success, "{}", result.err);
    assert!(result.out.contains("Type :help"), "{}", result.out);
    assert!(result.out.contains("root.nostdb"), "{}", result.out);
}

#[test]
fn help_lists_every_colon_command_the_repl_accepts() {
    let dir = project("help");
    let result = session(&dir, ":help\n:quit\n");
    for command in [
        ":help",
        ":begin",
        ":commit",
        ":rollback",
        ":database",
        ":quit",
    ] {
        assert!(result.out.contains(command), "{command} is missing");
    }
}

#[test]
fn a_statement_may_span_lines_and_ends_at_the_semicolon() {
    let dir = project("multiline");
    let result = session(
        &dir,
        "CREATE (n:Function\n  {name: \"login\"});\nMATCH (n) RETURN n.name;\n:quit\n",
    );
    assert_eq!(result.class, ExitClass::Success, "{}", result.err);
    let found = documents(&result.out);
    assert_eq!(found.len(), 2, "{}", result.out);
    assert_eq!(found[0]["summary"]["writes"]["nodes_created"], 1);
    assert_eq!(found[1]["rows"][0][0], "login");
}

#[test]
fn a_semicolon_inside_a_string_does_not_end_the_statement() {
    let dir = project("semicolon-in-string");
    let result = session(&dir, "RETURN \"a;b\" AS v;\n:quit\n");
    assert_eq!(result.class, ExitClass::Success, "{}", result.err);
    let found = documents(&result.out);
    assert_eq!(found[0]["rows"][0][0], "a;b", "{}", result.out);
}

#[test]
fn a_refused_statement_does_not_end_the_session() {
    let dir = project("recover");
    let result = session(&dir, "RETURN nonsense.x;\nRETURN 1 AS v;\n:quit\n");
    assert_eq!(result.class, ExitClass::Success);
    assert!(
        result.err.contains("CYPHER_SEMANTIC_ERROR"),
        "{}",
        result.err
    );
    // The session carried on and answered the next statement.
    let found = documents(&result.out);
    assert_eq!(
        found.last().map(|d| d["rows"][0][0].clone()),
        Some(1.into())
    );
}

#[test]
fn a_committed_transaction_keeps_its_work() {
    let dir = project("commit");
    let result = session(
        &dir,
        ":begin\nCREATE (n:Function {name: \"kept\"});\n:commit\nMATCH (n) RETURN n.name;\n:quit\n",
    );
    assert_eq!(result.class, ExitClass::Success, "{}", result.err);
    assert!(result.out.contains("transaction open"), "{}", result.out);
    assert!(
        result.out.contains("committed at generation"),
        "{}",
        result.out
    );
    let found = documents(&result.out);
    assert_eq!(found.last().unwrap()["rows"][0][0], "kept");
}

#[test]
fn a_rolled_back_transaction_keeps_nothing() {
    let dir = project("rollback");
    let result = session(
        &dir,
        ":begin\nCREATE (n:Function {name: \"discarded\"});\n:rollback\nMATCH (n) RETURN n.name;\n:quit\n",
    );
    assert_eq!(result.class, ExitClass::Success, "{}", result.err);
    assert!(result.out.contains("rolled back"), "{}", result.out);
    let found = documents(&result.out);
    assert_eq!(
        found.last().unwrap()["summary"]["rows"],
        0,
        "the rollback discarded the write:\n{}",
        result.out
    );
}

#[test]
fn leaving_with_a_transaction_open_rolls_it_back() {
    // Committing work the author never confirmed would be the worse guess.
    let dir = project("quit-open");
    let result = session(&dir, ":begin\nCREATE (n:Function {name: \"x\"});\n:quit\n");
    assert_eq!(result.class, ExitClass::Success);
    assert!(result.err.contains("rolled back"), "{}", result.err);

    let after = session(&dir, "MATCH (n) RETURN n.name;\n:quit\n");
    let found = documents(&after.out);
    assert_eq!(found.last().unwrap()["summary"]["rows"], 0);
}

#[test]
fn end_of_input_inside_a_transaction_also_rolls_it_back() {
    let dir = project("eof-open");
    let result = session(&dir, ":begin\nCREATE (n:Function {name: \"x\"});\n");
    assert_eq!(result.class, ExitClass::Success);
    assert!(result.err.contains("rolled back"), "{}", result.err);

    let after = session(&dir, "MATCH (n) RETURN n.name;\n:quit\n");
    assert_eq!(documents(&after.out).last().unwrap()["summary"]["rows"], 0);
}

#[test]
fn an_unterminated_statement_at_end_of_input_is_discarded_rather_than_run() {
    let dir = project("unterminated");
    let result = session(&dir, "CREATE (n:Function {name: \"never\"})\n");
    assert_eq!(result.class, ExitClass::Success);
    assert!(result.err.contains("unterminated"), "{}", result.err);

    let after = session(&dir, "MATCH (n) RETURN n.name;\n:quit\n");
    assert_eq!(
        documents(&after.out).last().unwrap()["summary"]["rows"],
        0,
        "a truncated write is worse than no write"
    );
}

#[test]
fn commit_and_rollback_outside_a_transaction_are_refused_without_ending_the_session() {
    let dir = project("stray-controls");
    let result = session(&dir, ":commit\n:rollback\nRETURN 1 AS v;\n:quit\n");
    assert_eq!(result.class, ExitClass::Success);
    assert_eq!(
        result.err.matches("no transaction is open").count(),
        2,
        "{}",
        result.err
    );
    assert!(!documents(&result.out).is_empty(), "the session carried on");
}

#[test]
fn begin_inside_a_transaction_is_refused_rather_than_nesting() {
    let dir = project("nested-begin");
    let result = session(&dir, ":begin\n:begin\n:rollback\n:quit\n");
    assert_eq!(result.class, ExitClass::Success);
    assert!(result.err.contains("already open"), "{}", result.err);
}

#[test]
fn an_unknown_colon_command_is_refused_in_both_loops() {
    let dir = project("unknown-command");
    let result = session(&dir, ":frobnicate\n:begin\n:frobnicate\n:rollback\n:quit\n");
    assert_eq!(result.class, ExitClass::Success);
    assert_eq!(
        result.err.matches("unknown command").count(),
        2,
        "{}",
        result.err
    );
}
