use std::path::PathBuf;

fn main() {
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

fn locate_schema() -> PathBuf {
    if let Ok(p) = std::env::var("CAPNP_SCHEMA") {
        return PathBuf::from(p);
    }
    // Default: sibling checkout of the C++ Cap'n Proto repo.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidate = manifest
        .parent()
        .expect("workspace parent")
        .join("capnproto/c++/src/capnp/schema.capnp");
    if candidate.exists() {
        return candidate;
    }
    panic!(
        "could not find schema.capnp; set CAPNP_SCHEMA env var. Tried: {}",
        candidate.display()
    );
}
