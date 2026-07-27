# nostdb-cli

`nostdb-cli` is the NostDB command-line interface. It owns the command surface, the
REPL, the output formats, embedded Engine integration, and the only native plugin
manager.

NostDB is a clean-slate, local-first Property Graph Database for software
environments.

## Boundary

This repository is the user-facing surface. It calls the public `nostdb-core` API and
implements no database behavior of its own.

It owns:

- the command surface: `init`, `plan`, `build`, `apply`, `check`, `convert`, `export`,
  `sync`, `query`, `link`, `catalog`, `server`, `plugin`, and `view`;
- the multiline REPL and its transaction controls;
- table, JSON, JSONL, and CSV output, keeping data and diagnostics separated;
- the exit classes the root product contract defines;
- the native plugin manager, which exists once and here.

It does not own:

- a parser, storage engine, synchronizer, or query engine, which belong to
  `nostdb-core`;
- the daemon, catalog, or IPC layer, which belong to `nostdb-server`;
- the GitHub provider, which is a separate out-of-process executable;
- the `.nost` grammar and conformance fixtures, which belong to `nostdb-spec`.

A command that appears to need one of those calls the Engine instead. Duplicating any
of them here would create a second implementation of behavior the product contract
defines once.

## Current status

The first commands are implemented:

```text
nostdb help [COMMAND]
nostdb init [PATH]
nostdb check TARGET
nostdb convert INPUT OUTPUT
nostdb export --nost [PATH]
nostdb query [CYPHER] [--format FORMAT] [--project PATH]
nostdb --version [--json]
```

`query` runs one statement, or opens a REPL when none is given. The REPL reads Cypher
terminated by `;` across as many lines as needed and accepts `:help`, `:begin`,
`:commit`, `:rollback`, `:database`, and `:quit`.

Results are written as `table`, `json`, `jsonl`, or `csv`. Only `json` and `jsonl` carry
the warnings; the other two write them to standard error, because a warning nobody sees
is the same as no warning. The table is for a person and is explicitly not stable.

`plan`, `build`, `apply`, `sync`, `link`, `catalog`, `server`, `plugin`, and
`view` are not implemented yet. See the implementation progress record in the root
superproject for which increment owns each.

## The Engine dependency

`nostdb-core` is a git dependency pinned to an exact commit rather than a branch. This
repository has to build without its siblings checked out, which rules out a path
dependency, and a reproducible build rules out following a branch. The verifier rejects
both, and a short commit hash too.

Raising the pin is a deliberate act: change the `rev` in `Cargo.toml`, run the verifier,
and record it in the root progress file alongside the submodule pin.

## Product contract

The normative product contract is the PRD in the root NostDB superproject at
<https://github.com/nostdb/nostdb>. Executable format, grammar, and protocol contracts
live in <https://github.com/nostdb/nostdb-spec>.

This repository keeps no copy of the PRD. A divergent child copy would create two
competing contracts.

## Verify

```bash
./scripts/verify-repository.sh
```

Continuous integration runs the same verifier on every push and pull request, so a
local pass and a CI pass check identical invariants.

## License

SSPL-1.0. See [LICENSE](LICENSE).

`nostdb-cli` is **source-available**, not open source. `nostdb-core` and
`nostdb-server` carry the same license. `nostdb-spec` and the Agent Skills are
Apache-2.0 so that any implementation can verify itself against the published
contracts.
