//! Semantic-token highlighting via tree-sitter-capnp's highlights query.
//!
//! We parse the buffer, run the bundled highlights query, then map each capture name
//! (e.g. `@type.builtin`, `@string`, `@field`) onto a fixed LSP semantic-token type/modifier
//! pair and encode them in the LSP-required relative-line / relative-start delta format.

use std::sync::OnceLock;

use ropey::Rope;
use tower_lsp::lsp_types::{
  SemanticToken, SemanticTokenModifier, SemanticTokenType,
};
use tree_sitter::{Parser, Query, QueryCursor, Tree};

/// The legend we advertise in `ServerCapabilities.semanticTokensProvider`. Indexes into
/// this list show up as `token_type` in the encoded tokens.
pub const TOKEN_TYPES: &[SemanticTokenType] = &[
  SemanticTokenType::KEYWORD,
  SemanticTokenType::TYPE,
  SemanticTokenType::STRUCT,
  SemanticTokenType::ENUM,
  SemanticTokenType::INTERFACE,
  SemanticTokenType::ENUM_MEMBER,
  SemanticTokenType::PROPERTY,
  SemanticTokenType::METHOD,
  SemanticTokenType::PARAMETER,
  SemanticTokenType::VARIABLE,
  SemanticTokenType::STRING,
  SemanticTokenType::NUMBER,
  SemanticTokenType::COMMENT,
  SemanticTokenType::OPERATOR,
  SemanticTokenType::DECORATOR,
  SemanticTokenType::NAMESPACE,
  SemanticTokenType::MACRO, // for $annotations applications
];

pub const TOKEN_MODIFIERS: &[SemanticTokenModifier] = &[
  SemanticTokenModifier::DECLARATION,
  SemanticTokenModifier::DEFINITION,
  SemanticTokenModifier::DEFAULT_LIBRARY,
  SemanticTokenModifier::DOCUMENTATION,
];

const MOD_DEFINITION: u32 = 1 << 1;
const MOD_DEFAULT_LIBRARY: u32 = 1 << 2;
#[allow(dead_code)]
const MOD_DOCUMENTATION: u32 = 1 << 3;

fn token_type_index(t: SemanticTokenType) -> u32 {
  TOKEN_TYPES.iter().position(|x| *x == t).unwrap() as u32
}

/// Map a tree-sitter highlight capture name to (LSP token type index, modifier bitmask).
/// Returns None for captures we choose not to render (e.g. punctuation).
fn capture_to_token(name: &str) -> Option<(u32, u32)> {
  use SemanticTokenType as T;
  let (ty, modifiers): (T, u32) = match name {
    "comment" => (T::COMMENT, 0),
    "string" | "string.special.path" => (T::STRING, 0),
    "number" | "constant.builtin" => (T::NUMBER, 0),
    "type" => (T::TYPE, 0),
    "type.builtin" => (T::TYPE, MOD_DEFAULT_LIBRARY),
    "type.qualifier" => (T::KEYWORD, 0),
    "type.definition" => (T::TYPE, MOD_DEFINITION),
    "keyword"
    | "keyword.import"
    | "keyword.directive"
    | "keyword.modifier"
    | "keyword.repeat"
    | "keyword.conditional"
    | "keyword.operator"
    | "keyword.function" => (T::KEYWORD, 0),
    "field" | "property" => (T::PROPERTY, 0),
    "method" | "function.method" => (T::METHOD, 0),
    "function" | "function.builtin" | "function.call" => (T::METHOD, 0),
    "variable" | "variable.parameter" => (T::VARIABLE, 0),
    "parameter" => (T::PARAMETER, 0),
    "operator" => (T::OPERATOR, 0),
    "label" => (T::DECORATOR, 0),
    "namespace" | "module" => (T::NAMESPACE, 0),
    "constant" => (T::ENUM_MEMBER, 0),
    "attribute" => (T::MACRO, 0),
    // Punctuation, delimiters, brackets — let the editor handle these natively.
    s if s.starts_with("punctuation") => return None,
    _ => return None,
  };
  Some((token_type_index(ty), modifiers))
}

fn highlights_query() -> &'static Query {
  static Q: OnceLock<Query> = OnceLock::new();
  Q.get_or_init(|| {
    Query::new(
      tree_sitter_capnp::language(),
      tree_sitter_capnp::HIGHLIGHTS_QUERY,
    )
    .expect("tree-sitter-capnp HIGHLIGHTS_QUERY failed to compile")
  })
}

pub fn parse(text: &str) -> Option<Tree> {
  let mut parser = Parser::new();
  parser.set_language(tree_sitter_capnp::language()).ok()?;
  parser.parse(text, None)
}

/// Compute LSP semantic tokens for the whole document. Returns tokens encoded in the
/// LSP-required deltaLine / deltaStart format.
pub fn full(text: &str) -> Vec<SemanticToken> {
  let Some(tree) = parse(text) else {
    return Vec::new();
  };
  let query = highlights_query();
  let rope = Rope::from_str(text);
  let mut cursor = QueryCursor::new();

  // Collect (start_line, start_col, length, ty_idx, mod_bitmask) absolute, then delta-encode.
  #[derive(Debug)]
  struct Tok {
    line: u32,
    col: u32,
    len: u32,
    ty: u32,
    modifiers: u32,
    // Used to deduplicate when multiple captures cover the same range — prefer the
    // most-specific (longest capture-name string) to mimic tree-sitter's own ordering.
    specificity: usize,
  }
  let mut toks: Vec<Tok> = Vec::new();

  for m in cursor.matches(query, tree.root_node(), text.as_bytes()) {
    for cap in m.captures {
      let name = &query.capture_names()[cap.index as usize];
      let Some((ty, modifiers)) = capture_to_token(name) else {
        continue;
      };
      let node = cap.node;
      let start = node.start_position();
      let end = node.end_position();
      // Only emit single-line tokens (LSP requires this).
      if start.row != end.row {
        // Split into per-line tokens.
        for row in start.row..=end.row {
          let line = rope.line(row);
          let col_start = if row == start.row { start.column } else { 0 };
          let col_end = if row == end.row {
            end.column
          } else {
            line.len_chars().saturating_sub(1) // exclude trailing newline
          };
          if col_end > col_start {
            toks.push(Tok {
              line: row as u32,
              col: col_start as u32,
              len: (col_end - col_start) as u32,
              ty,
              modifiers,
              specificity: name.len(),
            });
          }
        }
      } else {
        toks.push(Tok {
          line: start.row as u32,
          col: start.column as u32,
          len: (end.column - start.column) as u32,
          ty,
          modifiers,
          specificity: name.len(),
        });
      }
    }
  }

  // Sort by position, then specificity desc; dedupe by (line, col).
  toks.sort_by(|a, b| {
    (a.line, a.col)
      .cmp(&(b.line, b.col))
      .then_with(|| b.specificity.cmp(&a.specificity))
  });
  toks.dedup_by(|a, b| a.line == b.line && a.col == b.col);

  let mut out = Vec::with_capacity(toks.len());
  let mut prev_line = 0u32;
  let mut prev_col = 0u32;
  for t in toks {
    let delta_line = t.line - prev_line;
    let delta_start = if delta_line == 0 {
      t.col - prev_col
    } else {
      t.col
    };
    out.push(SemanticToken {
      delta_line,
      delta_start,
      length: t.len,
      token_type: t.ty,
      token_modifiers_bitset: t.modifiers,
    });
    prev_line = t.line;
    prev_col = t.col;
  }
  out
}
