# Third-party notices for the vendored grammar

The files in this directory are vendored from third-party projects and are **not**
covered by the top-level [`LICENSE`](../LICENSE) of this crate. Both are MIT
licensed, which is compatible with this crate's MIT licence. Their copyright and
permission notices are reproduced below as MIT requires.

These files are generated/maintained in the `vendor/tree-sitter-capnp` submodule
and copied in by `scripts/vendor-grammar.sh`; do not edit them here directly.

## `parser.c`, `highlights.scm`

Generated from / part of **tree-sitter-capnp**
(<https://github.com/amaanq/tree-sitter-capnp>). `parser.c` is generated from that
project's `grammar.js`; `highlights.scm` is copied from its `queries/`.

Licensed under the MIT License — full text in
[`LICENSE-tree-sitter-capnp`](./LICENSE-tree-sitter-capnp):

> Copyright (c) 2023 Amaan Qureshi <amaanq12@gmail.com>

## `tree_sitter/parser.h`

The tree-sitter runtime parser interface header, bundled with every generated
grammar and originating from **tree-sitter** (<https://github.com/tree-sitter/tree-sitter>).

Licensed under the MIT License:

```
Copyright (c) 2018 Max Brunsfeld

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
