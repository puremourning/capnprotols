# capnprotols

A language server for [Cap'n Proto](https://capnproto.org/) `.capnp` schema files.
Speaks LSP over stdio. Wraps the official `capnp` compiler for authoritative
diagnostics and symbol resolution, and uses [tree-sitter-capnp](https://github.com/amaanq/tree-sitter-capnp)
for editor-resilient highlighting.

## Features

- **Diagnostics** — parse and schema errors from `capnp compile`, mapped to LSP
  `Diagnostic`s with file/line/column ranges.
- **Go-to-definition** for types, enums, annotations, including:
  - cross-file via the compiler's `CodeGeneratorRequest` source info,
  - `using` alias redirects (local *and* `Receiver.Member` cross-file dotted refs),
  - the path string inside `import "..."`,
  - name-based fallback for cases the compiler doesn't track (e.g. type parameters
    inside `List(T)`).
- **Hover** — kind + name + the node's doc comment from `cgr.sourceInfo`.
- **Semantic-token highlighting** via tree-sitter-capnp's bundled queries, mapped
  to standard LSP token types (built-ins get the `defaultLibrary` modifier).
- **Completion** with cursor-context awareness:
  - after `:` / `(` / `,` → built-in primitives + user types,
  - after `$` → annotations,
  - after `Namespace.` → members of the imported file (uses index, falls back to
    a surface scan when no nodes from that import survived to the CGR),
  - in unknown contexts → top-level keywords (`struct`, `enum`, `interface`, …),
  - after `@` → the next valid field ordinal in the enclosing struct's ID space
    (scoped correctly across groups, unions, and nested structs).
- **Signature help** for annotation applications (`$Foo.bar(field = :Type, …)`)
  and generic instantiations (`List(T)`, `MyStruct(A, B)`).
- **Formatting** (`textDocument/formatting`) — conservative whitespace
  normalisation derived from the [KJ style guide](https://github.com/capnproto/capnproto/blob/master/style-guide.md)
  and Kenton's canonical schemas:
  - 2-space indentation, brace-on-same-line, `name @N :Type` colon spacing,
  - blank line between top-level decls, trailing whitespace stripped, single
    final newline,
  - doc-comment blocks re-indent with their declaration but contents are
    preserved verbatim (no paragraph reflow),
  - hard configurable column limit (default 100): trailing inline comments
    get pushed onto a new line when they push past it; long
    `$Annotation(...)` chains break before each `$`; long generic argument
    lists break inside `(...)` one arg per line,
  - long lines that don't match any wrapper produce a `WARNING` diagnostic,
  - `# capnpfmt: off` / `# capnpfmt: on` markers preserve a region verbatim,
  - bails (returns no edits) on any parse error so broken buffers aren't
    destructively rewritten.
- **Live-buffer overlay** — analysis runs on unsaved edits. The cached symbol
  index is retained across compile failures so completion and goto stay useful
  while you have a syntax error mid-edit.

## Build Requirements

- Rust toolchain (build only): `cargo`, `rustc`.
- A Cap'n Proto installation: the `capnp` binary on `$PATH` and its
  `capnp/schema.capnp` available under one of the install's include directories
  (Homebrew, MacPorts, apt and most manual installs put it there automatically).
  Tested with 1.3.0.

`build.rs` regenerates the Rust bindings from the installed `schema.capnp` so
the server gets the latest `startByte`/`endByte` and `FileSourceInfo` accessors.
Override the search with `CAPNP_SCHEMA=/path/to/schema.capnp` if needed.

## Build

```sh
cargo build --release
# binary at target/release/capnprotols
```

Or install it onto `$PATH` (under `~/.cargo/bin/`):

```sh
cargo install --path .
# binary at ~/.cargo/bin/capnprotols
```

## Configuration

Settings are passed via `initializationOptions` on the `initialize` request.
JSON shape:

```jsonc
{
  "compilerPath": "capnp",            // path to the capnp binary; default "capnp" on $PATH
  "importPaths":  ["/abs/dir/one"],   // extra -I paths for `import "/..."` resolution
  "format": {
    "enabled":        true,           // master switch for textDocument/formatting
    "maxWidth":       100,            // hard column limit (KJ style guide default)
    "warnLongLines":  true            // diagnose lines we can't auto-wrap
  }
}
```

Standard import roots are auto-discovered on startup. The server probes:

1. user-supplied `importPaths`,
2. `<install_prefix>/include` derived from the resolved `capnp` binary,
3. capnp's hardcoded paths (`/usr/local/include`, `/usr/include`),
4. common platform defaults (`/opt/homebrew/include`, `/opt/local/include`).

Each non-user candidate is kept only if it actually contains
`capnp/c++.capnp`. This covers Homebrew (Apple Silicon and Intel), MacPorts,
apt, and most manual installs without configuration.

### Logging

`CAPNPROTOLS_LOG=info` (or `debug`, `trace`) enables `tracing`-style logs on
stderr. The default is `info`. Logs go to stderr only — stdout is reserved for
the LSP framing.

## Editor setup

The server speaks vanilla LSP over stdio with no custom extensions. Any LSP
client works.

### YouCompleteMe (ycmd)

Add to your `.vimrc`:

```python
let g:ycm_langauge_server += [
    \   {
    \     'name': 'capnprotols',
    \     'cmdline': [ '/path/to/capnprotols' ],
    \     'filetypes': [ 'capnp' ],
    \   },
]
```

### VS Code

A minimal extension is included under [`extension/`](extension/) — a thin
LSP client that launches the `capnprotols` binary. See its
[README](extension/README.md) for build/install steps.

## Architecture notes

- `compiler.rs` shells out to `capnp compile -o-` against an overlay file written
  alongside the original (so relative imports still resolve), then path-remaps
  the compiler-reported overlay path back to the real file.
- `index.rs` decodes the CGR into a per-file FSI table (sorted byte-ranges →
  resolved typeIds) plus a per-node table (kind, displayName, fields, generic
  parameters, doc comment, source byte range).
- `aliases.rs` handles `using NAME = …` and surface-scans top-level declarations
  for cases where a referenced file isn't represented in the current CGR.
- `ordinals.rs` brace-tracks the buffer to find the enclosing struct and compute
  the next contiguous `@<n>` ordinal.
- `semantic_tokens.rs` runs tree-sitter-capnp's `HIGHLIGHTS_QUERY` and emits LSP
  semantic tokens.
- `server.rs` wires everything into `tower-lsp` and detects cursor contexts
  (type / annotation / member / field-ordinal slots) for completion and
  signature help.

## License

MIT.
