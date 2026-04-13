// Re-export the build-time generated `schema.capnp` bindings under a stable path.
#![allow(clippy::all)]
#![allow(dead_code, non_snake_case, unused_imports, unused_qualifications)]

include!(concat!(env!("OUT_DIR"), "/schema_capnp.rs"));
