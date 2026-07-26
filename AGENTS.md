# nostdb-cli Agent Instructions

## Inheritance

This repository is a child of the NostDB root superproject. The root `AGENTS.md`
at <https://github.com/nostdb/nostdb> is the governing contract.

This file only narrows the root rules for the command-line boundary. It must not
weaken any root product, safety, or ownership boundary. If this file and the root
contract appear to conflict, the root contract wins, the current valid behavior stays
unchanged, and the exact conflict is recorded in the root
`IMPLEMENTATION_PROGRESS.md`.

## Language policy

Write everything in this repository in English only.

This covers documentation, source code, identifiers, comments, rustdoc, test names,
commit messages, branch names, pull request titles and bodies, issue text, diagnostics,
error messages, log records, configuration, fixtures, help text, and every line the
command surface prints.

This rule holds regardless of the language a request is written in.

## Ownership boundary

`nostdb-cli` is the user-facing surface and implements no database behavior.

Permitted:

- the command surface and its argument parsing;
- the multiline REPL, including its transaction controls;
- table, JSON, JSONL, and CSV output;
- the exit classes;
- embedded Engine integration, and later the daemon client;
- the native plugin manager, which exists once and here.

Prohibited:

- a parser, storage engine, synchronizer, analyzer, or query engine;
- any `.nostdb` writer, because only `nostdb-core` writes `.nostdb`;
- a daemon, named-database catalog, or IPC transport, which belong to `nostdb-server`;
- a bundled GitHub provider implementation;
- a second copy of the `.nost` grammar or the conformance fixtures;
- a copy of the root PRD;
- code copied in from any legacy implementation.

If a command appears to need one of the prohibited items, it needs a public
`nostdb-core` API instead. Add the API there and call it.

## Invariants this repository must never break

- Only the Engine writes `.nostdb`. Every mutation goes through a public Core API.
- A path-based command works in Embedded Mode with no daemon running.
- Machine-readable output keeps data on stdout and diagnostics on stderr.
- A warning never silently changes what a command did.
- Unsupported query syntax is reported with its source range and never run under a
  guessed alternative.
- Result order is undefined without `ORDER BY`, and no output format may imply
  otherwise.
- Secrets never reach a log record, a diagnostic, command output, a settings file, or a
  plugin lock.
- A failed command preserves the last valid database generation.
- Exit classes are stable. A symbolic diagnostic code is the normative signal, and the
  numeric class follows the root product contract.

## Rust standards

Rust stable and Edition 2024. Public APIs require explicit error types and rustdoc. Use
`#![forbid(unsafe_code)]` where practical; required `unsafe` code needs a separate ADR
with documented safety invariants and a Miri or equivalent verification plan before
implementation.

This repository owns a binary, so it is the one place in the workspace that maps an
error to a process exit code. It still returns typed errors internally and converts at
the boundary.

Every change must pass:

```bash
cargo fmt --check
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

Do not add a dependency without documenting its purpose, maintenance status, and
license.

## Repository verification

Run before every commit:

```bash
./scripts/verify-repository.sh
```

The verifier is non-mutating. Extend it as the command surface lands rather than
replacing it with a manual checklist.

## Testing expectations

Treat every argument, every file path, and every query string as untrusted input.

Each boundary carries its own coverage:

- commands: exit classes, argument errors, and help text;
- output: JSON, JSONL, CSV, and table, with data and diagnostics separated;
- REPL: multiline input, the transaction controls, and interrupted input;
- conversion: both directions, and a refusal that changes neither representation;
- links: add, remove, list, check, and refresh, including an unavailable target;
- plugins: manifest validation, pinning, integrity, consent, and failure preservation.

A command that changes state without a test proving what it preserves on failure is
incomplete.

## Safety and external actions

- Never execute analyzed source code.
- Do not create remote repositories, add remotes, push to a new remote, publish
  packages, create releases, or modify registries without explicit user authorization.
- Never place credentials, passwords, tokens, private keys, or PEM content in files,
  fixtures, diagnostics, or command output.
- Do not use destructive Git commands or broad deletion.
- Preserve existing user changes and never revert them without authorization.

## Stage workflow

Implementation sequencing is tracked in the root `IMPLEMENTATION_PROGRESS.md`, not in
this repository. Do not begin a later Stage during a setup-only request, and do not mark
a Stage `DONE` until every Acceptance Criterion passes.
