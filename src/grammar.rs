//! The vendored tree-sitter Cap'n Proto grammar.
//!
//! `grammar/parser.c` is copied out of the `vendor/tree-sitter-capnp` submodule by
//! `scripts/vendor-grammar.sh` and compiled by `build.rs`, so the published crate is
//! self-contained (crates.io does not package submodules and forbids path/git deps).
//! This module is the crate-local replacement for the external `tree-sitter-capnp`
//! crate — it exposes the parser [`language`] and the vendored highlight query.

use tree_sitter::Language;

extern "C" {
  fn tree_sitter_capnp() -> Language;
}

/// The tree-sitter [`Language`] for Cap'n Proto schemas.
pub fn language() -> Language {
  unsafe { tree_sitter_capnp() }
}

/// The syntax-highlighting query, vendored from the grammar's `queries/highlights.scm`.
pub const HIGHLIGHTS_QUERY: &str = include_str!("../grammar/highlights.scm");
