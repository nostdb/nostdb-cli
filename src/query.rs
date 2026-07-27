//! `nostdb query`, in immediate mode and as a REPL.
//!
//! Neither parses Cypher nor executes it. Both call `nostdb-core`, render the envelope it
//! returns through [`crate::output`], and choose an exit class.
//!
//! # Why the transaction loop is nested
//!
//! A `Transaction` borrows the `Database` for as long as it lives, so the REPL cannot
//! hold one in a variable across iterations of a loop that also owns the database. `:begin`
//! therefore enters a second loop that owns the transaction and returns when it commits or
//! rolls back.
//!
//! That is not a workaround. It makes the transaction's extent a lexical region, so there
//! is no state in which the REPL believes it is in a transaction and is not.

use crate::exit::ExitClass;
use crate::output::{self, Format};
use nostdb_core::QueryError;
use nostdb_core::cypher::{Query, parse};
use nostdb_core::diagnostic::Severity;
use nostdb_core::execute::{LinkedSource, Parameters};
use nostdb_core::federation::Federation;
use nostdb_core::project::Project;
use nostdb_core::result::ResultEnvelope;
use nostdb_core::storage::Database;
use nostdb_core::transaction::{Transaction, TransactionError};
use std::io::{BufRead, Write};
use std::path::Path;

/// The exit class a query failure reports.
///
/// Unsupported syntax is a usage failure, because the caller wrote something outside the
/// published subset and the remedy is to write something else. A semantic error is
/// validation: the query is inside the subset and wrong.
fn query_class(error: &QueryError) -> ExitClass {
    use nostdb_core::diagnostic::DiagnosticCode;
    match error.code {
        DiagnosticCode::CypherUnsupported => ExitClass::Usage,
        DiagnosticCode::LinkedDatabaseReadOnly => ExitClass::Validation,
        _ => ExitClass::Validation,
    }
}

fn transaction_class(error: &TransactionError) -> ExitClass {
    match error {
        TransactionError::Conflict { .. } => ExitClass::Conflict,
        TransactionError::Storage(_) => ExitClass::Io,
        TransactionError::Decode(_) => ExitClass::Validation,
        TransactionError::Query(query) => query_class(query),
    }
}

fn report_query_error(error: &QueryError, err: &mut dyn Write) {
    let range = error.range;
    let position = format!("{}:{}: ", range.start().line, range.start().column);
    let _ = writeln!(
        err,
        "{position}error: {}: {}",
        error.code.as_str(),
        error.message
    );
}

/// Writes an envelope, and its warnings when the format cannot carry them.
fn emit(envelope: &ResultEnvelope, format: Format, out: &mut dyn Write, err: &mut dyn Write) {
    let _ = output::write(envelope, format, out);
    if !format.carries_warnings() {
        for warning in &envelope.warnings {
            let _ = writeln!(
                err,
                "{}: {}: {}",
                warning.severity,
                warning.code.as_str(),
                warning.message
            );
        }
    }
}

/// Opens the database a query should run against, and resolves its links.
///
/// The federation is resolved once per invocation rather than per statement. A query
/// snapshot pins each linked source, which is what the product contract requires: a
/// second statement in the same session must not see a link that changed underneath it.
fn open(from: &Path, err: &mut dyn Write) -> Result<(Database, Federation), ExitClass> {
    let global = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| Path::new(&home).join(".nostdb").join("settings.json"));
    let project = Project::discover(from, global.as_deref()).map_err(|error| {
        let _ = writeln!(err, "{error}");
        ExitClass::for_project_error(&error)
    })?;
    let federation = project.resolve_links().map_err(|error| {
        let _ = writeln!(err, "{error}");
        ExitClass::for_project_error(&error)
    })?;
    let database = project.open_database().map_err(|error| {
        let _ = writeln!(err, "{error}");
        ExitClass::for_project_error(&error)
    })?;
    Ok((database, federation))
}

/// The linked sources a query may read, taken from a resolved federation.
///
/// Index zero is the root, which the transaction already holds, so it is skipped.
fn linked_sources(federation: &Federation) -> Vec<LinkedSource<'_>> {
    federation
        .sources
        .iter()
        .skip(1)
        .filter_map(|source| {
            Some(LinkedSource {
                locator: source.locator.as_ref()?,
                graph: &source.graph,
            })
        })
        .collect()
}

/// `nostdb query CYPHER`, immediate mode.
///
/// One statement, one transaction, committed if it changed anything.
pub fn immediate(
    from: &Path,
    text: &str,
    format: Format,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitClass {
    let query = match parse(text) {
        Ok(query) => query,
        Err(error) => {
            report_query_error(&error, err);
            return query_class(&error);
        }
    };
    let (mut database, federation) = match open(from, err) {
        Ok(opened) => opened,
        Err(class) => return class,
    };
    run_one(&mut database, &federation, &query, format, out, err)
}

fn run_one(
    database: &mut Database,
    federation: &Federation,
    query: &Query,
    format: Format,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitClass {
    let linked = linked_sources(federation);
    let mut transaction = match Transaction::begin(database) {
        Ok(transaction) => transaction,
        Err(error) => {
            let _ = writeln!(err, "{error}");
            return transaction_class(&error);
        }
    };
    let result = match transaction.run_federated(query, &Parameters::new(), &linked) {
        Ok(result) => result,
        Err(error) => {
            report_query_error(&error, err);
            transaction.rollback();
            return query_class(&error);
        }
    };
    let writes = transaction.writes();
    let wrote = !writes.is_empty();
    let generation = match transaction.commit() {
        Ok(generation) => generation,
        Err(error) => {
            let _ = writeln!(err, "{error}");
            return transaction_class(&error);
        }
    };

    // A read reports no write summary at all, which is the distinction the result
    // contract draws between "changed nothing" and "could not change anything".
    let mut envelope = ResultEnvelope::new(result, generation, wrote.then_some(writes));
    describe_federation(&mut envelope, federation);
    emit(&envelope, format, out, err);
    ExitClass::Success
}

/// Fills the envelope's federation fields from a resolved set.
///
/// The warnings are appended rather than replaced, because a query may have produced its
/// own and both belong in the same envelope. `partial` follows from the three link codes,
/// so setting the warnings is what sets it.
fn describe_federation(envelope: &mut ResultEnvelope, federation: &Federation) {
    envelope.linked_databases_opened = federation.linked_databases_opened();
    envelope.warnings.extend(federation.warnings());
}

const REPL_HELP: &str = "\
Enter Cypher terminated by `;`. A statement may span lines.

  :help       show this
  :begin      start an explicit transaction
  :commit     commit the open transaction
  :rollback   discard the open transaction
  :database   show the database this session opened
  :quit       leave
";

/// Reads one statement, which may span lines, or one colon command.
///
/// Returns `None` at end of input. A statement is complete at the first `;` outside a
/// string literal; a colon command is complete at the end of its line.
fn read_statement(reader: &mut dyn BufRead, prompt: &str, out: &mut dyn Write) -> Option<String> {
    let mut buffer = String::new();
    loop {
        let _ = write!(out, "{}", if buffer.is_empty() { prompt } else { "... " });
        let _ = out.flush();

        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                // End of input. A partial statement is discarded rather than run: a
                // truncated write is worse than no write.
                return (!buffer.trim().is_empty()).then(String::new);
            }
            Ok(_) => {}
            Err(_) => return None,
        }

        if buffer.is_empty() && line.trim_start().starts_with(':') {
            return Some(line.trim().to_owned());
        }
        buffer.push_str(&line);

        if let Some(end) = terminator(&buffer) {
            let statement = buffer[..end].trim().to_owned();
            return Some(statement);
        }
    }
}

/// The offset of the first `;` outside a string literal, if the buffer holds one.
fn terminator(buffer: &str) -> Option<usize> {
    let mut in_string = false;
    let mut escaped = false;
    for (offset, character) in buffer.char_indices() {
        match character {
            _ if escaped => escaped = false,
            '\\' if in_string => escaped = true,
            '"' | '\'' => in_string = !in_string,
            ';' if !in_string => return Some(offset),
            _ => {}
        }
    }
    None
}

/// `nostdb query`, interactive mode.
///
/// Reads from `input` rather than the terminal, so a test drives the whole session.
pub fn repl(
    from: &Path,
    format: Format,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitClass {
    let (mut database, federation) = match open(from, err) {
        Ok(opened) => opened,
        Err(class) => return class,
    };
    let path = database.path().display().to_string();
    let _ = writeln!(out, "nostdb {} — {path}", crate::VERSION);
    let _ = writeln!(out, "Type :help for commands, :quit to leave.");

    loop {
        let Some(statement) = read_statement(input, "nostdb> ", out) else {
            return ExitClass::Success;
        };
        if statement.is_empty() {
            let _ = writeln!(err, "warning: discarded an unterminated statement");
            return ExitClass::Success;
        }
        match statement.as_str() {
            ":quit" | ":exit" => return ExitClass::Success,
            ":help" => {
                let _ = write!(out, "{REPL_HELP}");
            }
            ":database" => {
                let _ = writeln!(out, "{path}");
            }
            ":commit" | ":rollback" => {
                let _ = writeln!(err, "error: no transaction is open; use :begin first");
            }
            ":begin" => {
                if let Some(class) =
                    transaction_session(&mut database, &federation, format, input, out, err)
                {
                    return class;
                }
            }
            other if other.starts_with(':') => {
                let _ = writeln!(err, "error: unknown command `{other}`; try :help");
            }
            text => {
                let query = match parse(text) {
                    Ok(query) => query,
                    Err(error) => {
                        report_query_error(&error, err);
                        continue;
                    }
                };
                run_one(&mut database, &federation, &query, format, out, err);
            }
        }
    }
}

/// Runs statements inside one explicit transaction.
///
/// Returns `Some(class)` when the session ended inside the transaction, and `None` when it
/// closed and the outer loop should continue.
fn transaction_session(
    database: &mut Database,
    federation: &Federation,
    format: Format,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Option<ExitClass> {
    let mut transaction = match Transaction::begin(database) {
        Ok(transaction) => transaction,
        Err(error) => {
            let _ = writeln!(err, "{error}");
            return Some(transaction_class(&error));
        }
    };
    let linked = linked_sources(federation);
    let base = transaction.base_generation();
    let _ = writeln!(out, "transaction open at generation {}", base.get());

    loop {
        let Some(statement) = read_statement(input, "nostdb*> ", out) else {
            // End of input inside a transaction discards it. Committing work the author
            // never confirmed would be the worse guess.
            transaction.rollback();
            let _ = writeln!(
                err,
                "warning: input ended, so the transaction was rolled back"
            );
            return Some(ExitClass::Success);
        };
        match statement.as_str() {
            "" => {
                transaction.rollback();
                let _ = writeln!(
                    err,
                    "warning: discarded an unterminated statement, so the transaction was \
                     rolled back"
                );
                return Some(ExitClass::Success);
            }
            ":quit" | ":exit" => {
                transaction.rollback();
                let _ = writeln!(
                    err,
                    "warning: left with a transaction open, so it was rolled back"
                );
                return Some(ExitClass::Success);
            }
            ":help" => {
                let _ = write!(out, "{REPL_HELP}");
            }
            ":begin" => {
                let _ = writeln!(err, "error: a transaction is already open");
            }
            ":rollback" => {
                transaction.rollback();
                let _ = writeln!(out, "rolled back");
                return None;
            }
            ":commit" => {
                let wrote = !transaction.writes().is_empty();
                match transaction.commit() {
                    Ok(generation) => {
                        let _ = writeln!(
                            out,
                            "committed at generation {}{}",
                            generation.get(),
                            if wrote { "" } else { " (nothing changed)" }
                        );
                        return None;
                    }
                    Err(error) => {
                        let _ = writeln!(err, "{error}");
                        return Some(transaction_class(&error));
                    }
                }
            }
            other if other.starts_with(':') => {
                let _ = writeln!(err, "error: unknown command `{other}`; try :help");
            }
            text => {
                let query = match parse(text) {
                    Ok(query) => query,
                    Err(error) => {
                        report_query_error(&error, err);
                        continue;
                    }
                };
                match transaction.run_federated(&query, &Parameters::new(), &linked) {
                    Ok(result) => {
                        // Inside a transaction nothing is committed yet, so the generation
                        // shown is the one the transaction began at.
                        let writes = transaction.writes();
                        let mut envelope = ResultEnvelope::new(
                            result,
                            base,
                            (!writes.is_empty()).then_some(writes),
                        );
                        describe_federation(&mut envelope, federation);
                        emit(&envelope, format, out, err);
                    }
                    Err(error) => report_query_error(&error, err),
                }
            }
        }
    }
}

/// Reports whether any diagnostic is an error rather than a warning.
#[must_use]
pub fn has_errors(found: &[nostdb_core::diagnostic::Diagnostic]) -> bool {
    found
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_statement_ends_at_the_first_semicolon_outside_a_string() {
        assert_eq!(terminator("RETURN 1;"), Some(8));
        assert_eq!(terminator("RETURN 1"), None);
        // A semicolon inside a string literal does not end the statement, which is the
        // whole reason this is not `find(';')`.
        assert_eq!(terminator("RETURN \"a;b\""), None);
        assert_eq!(terminator("RETURN \"a;b\";"), Some(12));
        assert_eq!(terminator("RETURN 'a;b';"), Some(12));
        // An escaped quote does not close the string.
        assert_eq!(terminator("RETURN \"a\\\";b\""), None);
    }

    fn read_all(input: &str) -> Vec<String> {
        let mut reader = std::io::BufReader::new(input.as_bytes());
        let mut out = Vec::new();
        let mut statements = Vec::new();
        while let Some(statement) = read_statement(&mut reader, "> ", &mut out) {
            if statement.is_empty() {
                statements.push(String::new());
                break;
            }
            statements.push(statement);
        }
        statements
    }

    #[test]
    fn a_statement_may_span_lines() {
        assert_eq!(
            read_all("MATCH (n)\nRETURN n\nLIMIT 1;\n"),
            vec!["MATCH (n)\nRETURN n\nLIMIT 1".to_owned()]
        );
    }

    #[test]
    fn a_colon_command_ends_at_its_line() {
        assert_eq!(
            read_all(":help\n:quit\n"),
            vec![":help".to_owned(), ":quit".to_owned()]
        );
    }

    #[test]
    fn an_unterminated_statement_at_end_of_input_is_discarded() {
        // Running it would be guessing where the author meant to stop, and a truncated
        // write is worse than no write.
        assert_eq!(read_all("CREATE (n:A)\n"), vec![String::new()]);
    }
}
