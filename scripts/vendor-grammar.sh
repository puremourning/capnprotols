#!/usr/bin/env bash
#
# Re-vendor the tree-sitter Cap'n Proto parser from the submodule into the
# crate's tracked `grammar/` directory.
#
# The published crate must be self-contained (crates.io forbids path/git deps
# and does not package submodule contents), so the generated C parser and the
# highlights query are copied out of the `vendor/tree-sitter-capnp` submodule
# and committed directly under `grammar/`. `build.rs` compiles `grammar/parser.c`.
#
# Run this after editing the grammar (which lives in the submodule). CI runs it
# too and fails if the committed copy has drifted from a fresh `tree-sitter
# generate` — see .github/workflows/ci.yml.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
submodule="$repo_root/vendor/tree-sitter-capnp"
dest="$repo_root/grammar"

if [[ ! -d "$submodule/src" ]]; then
  echo "error: submodule not checked out at $submodule" >&2
  echo "       run: git submodule update --init vendor/tree-sitter-capnp" >&2
  exit 1
fi

mkdir -p "$dest/tree_sitter"
cp "$submodule/src/parser.c" "$dest/parser.c"
cp "$submodule/src/tree_sitter/parser.h" "$dest/tree_sitter/parser.h"
cp "$submodule/queries/highlights.scm" "$dest/highlights.scm"
# MIT requires the copyright + permission notice travel with the copied code.
# Ship the grammar's licence verbatim; parser.h (tree-sitter core) is covered by
# grammar/THIRD_PARTY_NOTICES.md.
cp "$submodule/LICENSE" "$dest/LICENSE-tree-sitter-capnp"

echo "Vendored grammar into $dest:"
echo "  parser.c                 <- src/parser.c"
echo "  tree_sitter/parser.h     <- src/tree_sitter/parser.h"
echo "  highlights.scm           <- queries/highlights.scm"
echo "  LICENSE-tree-sitter-capnp <- LICENSE"
