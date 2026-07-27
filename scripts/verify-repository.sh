#!/usr/bin/env bash

# Non-mutating verification for nostdb-cli.
#
# Covers the repository shape, the ownership boundaries in AGENTS.md, the Engine
# dependency pin, and the Rust command set.

set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repository_root=$(CDPATH= cd -- "$script_dir/.." && pwd)

cd "$repository_root"

required_files="
AGENTS.md
CLAUDE.md
README.md
LICENSE
.gitignore
.editorconfig
.github/workflows/verify.yml
Cargo.toml
Cargo.lock
rust-toolchain.toml
src/main.rs
src/lib.rs
"

for required_file in $required_files; do
  if [ ! -e "$required_file" ]; then
    echo "missing required file: $required_file" >&2
    exit 1
  fi
done

# LICENSE is verbatim upstream text and is intentionally not whitespace-scanned.
checked_text_files="
AGENTS.md
README.md
.gitignore
.editorconfig
.github/workflows/verify.yml
Cargo.toml
rust-toolchain.toml
scripts/verify-repository.sh
"

for checked_file in $checked_text_files; do
  if grep -nE '[[:blank:]]+$' "$checked_file"; then
    echo "trailing whitespace found in: $checked_file" >&2
    exit 1
  fi
done

if [ ! -L CLAUDE.md ] || [ "$(readlink CLAUDE.md)" != "AGENTS.md" ]; then
  echo "CLAUDE.md must be a symlink to AGENTS.md" >&2
  exit 1
fi

if ! grep -q '^ *Server Side Public License$' LICENSE; then
  echo "LICENSE must be the Server Side Public License, Version 1" >&2
  exit 1
fi

if ! grep -q '^ *VERSION 1, OCTOBER 16, 2018$' LICENSE; then
  echo "LICENSE must be the Server Side Public License, Version 1" >&2
  exit 1
fi

# Section 13 is the clause that distinguishes the SSPL from the GPL family.
# Requiring it also detects a truncated license file.
if ! grep -q 'Offering the Program as a Service' LICENSE; then
  echo "LICENSE is missing Server Side Public License section 13" >&2
  exit 1
fi

# The CLI must not grow a second Engine. These are the ownership boundaries in
# AGENTS.md, and nothing else checks them. They run now rather than with the crate,
# so the first command added cannot quietly bring a parser with it.
if [ -d src ] && grep -rnE '\b(TcpListener|UnixListener|HttpServer)\b' src >/dev/null 2>&1; then
  echo "nostdb-cli must not contain a network or IPC listener; the daemon is nostdb-server" >&2
  exit 1
fi

if [ -e grammar ] || [ -e fixtures ]; then
  echo "the grammar and the conformance fixtures belong to nostdb-spec" >&2
  exit 1
fi

if [ -e docs/PRD.md ]; then
  echo "the PRD lives once, in the root superproject" >&2
  exit 1
fi

# The Engine dependency is pinned to an exact commit. docs/REPOSITORIES.md requires a
# reproducible build and forbids following a floating branch, and a `branch =` or `tag =`
# dependency would do exactly that. A path dependency would break this repository's
# promise to build without its siblings, which is what CI checks out.
if grep -q 'nostdb-core' Cargo.toml; then
  if ! grep -qE '^nostdb-core = \{ git = "https://github.com/nostdb/nostdb-core\.git", rev = "[0-9a-f]{40}" \}$' Cargo.toml; then
    echo "nostdb-core must be pinned to an exact 40-character commit over https" >&2
    exit 1
  fi
fi

git diff --check

if [ -f Cargo.toml ]; then
  if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo is required to verify the command surface" >&2
    exit 1
  fi
  cargo fmt --check
  cargo check --all-targets --all-features
  cargo clippy --all-targets --all-features -- -D warnings
  # The daemon round trip binds this user's real endpoint, so it is opt-in. It declines when a
  # daemon is already running rather than stopping one it did not start, which is what makes it
  # safe to switch on here and keeps a local run checking the same invariants as CI.
  NOSTDB_DAEMON_TEST=1 cargo test --all-targets --all-features
fi

echo "nostdb-cli verification passed"
