use std::path::PathBuf;

fn main() {
  compile_grammar();

  // Regenerate schema_capnp.rs from the C++ repo's schema.capnp so we get the latest
  // accessors (startByte/endByte on Node, FileSourceInfo on RequestedFile, etc.) which
  // older versions of the published `capnp` crate's bundled bindings lack.
  let schema = locate_schema();
  println!("cargo:rerun-if-changed={}", schema.display());
  println!("cargo:rerun-if-env-changed=CAPNP_SCHEMA");

  let parent = schema.parent().expect("schema parent");
  capnpc::CompilerCommand::new()
    .src_prefix(parent)
    .file(&schema)
    .output_path(PathBuf::from(std::env::var("OUT_DIR").unwrap()))
    .run()
    .expect("capnpc failed to compile schema.capnp — is `capnp` on $PATH?");
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

fn locate_schema() -> PathBuf {
  if let Ok(p) = std::env::var("CAPNP_SCHEMA") {
    return PathBuf::from(p);
  }
  // Probe the same include roots `capnp` itself searches, plus the install prefix
  // derived from the resolved binary location and a few common platform defaults.
  let mut roots: Vec<PathBuf> = Vec::new();
  if let Some(inc) = capnp_install_include() {
    roots.push(inc);
  }
  roots.extend(
    [
      "/usr/local/include",
      "/usr/include",
      "/opt/homebrew/include",
      "/opt/local/include",
    ]
    .iter()
    .map(PathBuf::from),
  );
  // Last resort: a sibling checkout of the C++ repo (handy for hacking on capnproto
  // itself before installing).
  if let Some(parent) = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent() {
    roots.push(parent.join("capnproto/c++/src"));
  }
  for root in &roots {
    let candidate = root.join("capnp/schema.capnp");
    if candidate.exists() {
      return candidate;
    }
  }
  panic!(
    "could not find capnp/schema.capnp in any standard include directory; \
         set CAPNP_SCHEMA env var. Tried: {}",
    roots
      .iter()
      .map(|p| p.display().to_string())
      .collect::<Vec<_>>()
      .join(", ")
  );
}

fn capnp_install_include() -> Option<PathBuf> {
  let path_var = std::env::var_os("PATH")?;
  for dir in std::env::split_paths(&path_var) {
    let candidate = dir.join("capnp");
    if candidate.is_file() {
      let inc = dir.parent()?.join("include");
      if inc.is_dir() {
        return Some(inc);
      }
    }
  }
  None
}
