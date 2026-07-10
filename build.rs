use std::path::PathBuf;

fn main() {
  compile_grammar();
}

/// Compile the vendored tree-sitter Cap'n Proto parser.
///
/// The generated `grammar/parser.c` is copied out of the `vendor/tree-sitter-capnp`
/// submodule by `scripts/vendor-grammar.sh` and committed directly, so the published
/// crate is self-contained (no path/git/submodule dependency). The parser has no
/// external scanner, so a C compiler is the only build requirement.
fn compile_grammar() {
  let grammar_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("grammar");
  let parser = grammar_dir.join("parser.c");
  println!("cargo:rerun-if-changed={}", parser.display());
  cc::Build::new()
    .include(&grammar_dir)
    .file(&parser)
    .flag_if_supported("-Wno-unused-parameter")
    .flag_if_supported("-Wno-unused-but-set-variable")
    .flag_if_supported("-Wno-trigraphs")
    .compile("tree_sitter_capnp");
}
