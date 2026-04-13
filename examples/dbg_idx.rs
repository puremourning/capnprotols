use std::path::PathBuf;
use std::process::Command;

#[path = "../src/schema_capnp.rs"]
mod schema_capnp;
#[path = "../src/index.rs"]
mod index;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/Users/ben/Development/capnproto/test/foo.capnp".to_string());
    let out = Command::new("capnp")
        .args(["compile", "-o-", &path])
        .output()
        .unwrap();
    println!("cgr bytes: {}", out.stdout.len());
    let idx = index::Index::from_cgr_bytes(&out.stdout).unwrap();
    println!("nodes: {}", idx.nodes.len());
    println!("files keys:");
    for (p, fi) in &idx.files {
        println!("  {:?} -> {} idents", p, fi.identifiers.len());
        for i in &fi.identifiers[..3.min(fi.identifiers.len())] {
            println!("    {}-{} -> {}", i.start_byte, i.end_byte, i.target_node_id);
        }
    }
    let path = PathBuf::from(&path);
    for byte in [46, 102, 155, 300] {
        println!("lookup byte {byte}: {:?}", idx.identifier_at(&path, byte));
    }
}
