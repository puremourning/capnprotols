// Re-export the `capnp` crate's own `schema.capnp` bindings under a stable path.
//
// We used to regenerate these from a system `schema.capnp` at build time (via build.rs)
// because older published `capnp` crates shipped bindings that lacked accessors we need
// (startByte/endByte, FileSourceInfo, etc.). The pinned `capnp` dependency now bundles an
// up-to-date `schema_capnp` — including the `type` alias (newtype) Node variant — so we
// simply re-export it. This keeps the LSP's understanding of the schema exactly in step
// with the `capnp` crate it links against.
#![allow(clippy::all)]
#![allow(dead_code, non_snake_case, unused_imports, unused_qualifications)]

pub use capnp::schema_capnp::*;
