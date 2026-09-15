//! Lightweight scanner for `using NAME = TYPE;` declarations. Cap'n Proto's compiler
//! resolves `using` aliases away in the CodeGeneratorRequest, so go-to-definition on an
//! alias name needs to consult the source text directly.

use std::sync::OnceLock;

use regex::Regex;

#[derive(Debug, Clone)]
pub struct UsingAlias {
  pub name:            String,
  pub name_start_byte: usize,
  pub name_end_byte:   usize,
}

fn re() -> &'static Regex {
  static R: OnceLock<Regex> = OnceLock::new();
  // `using NAME = ...;` — capnp also allows `using import "..."`, which doesn't bind a
  // single name in the same way; we skip that form. Comments are stripped naively below.
  R.get_or_init(|| {
    Regex::new(r"\busing\s+([A-Za-z_][A-Za-z0-9_]*)\s*=").unwrap()
  })
}

/// Strip line comments (`# ...`) so we don't match `using` inside a comment. Cheap pass —
/// we replace comment chars with spaces to preserve byte offsets.
pub(crate) fn strip_comments(src: &str) -> String {
  let mut out = src.as_bytes().to_vec();
  let mut i = 0;
  while i < out.len() {
    if out[i] == b'#' {
      while i < out.len() && out[i] != b'\n' {
        out[i] = b' ';
        i += 1;
      }
    } else {
      i += 1;
    }
  }
  // Safe: we only replaced ASCII bytes with ASCII spaces.
  String::from_utf8(out).unwrap_or_default()
}

pub fn scan(src: &str) -> Vec<UsingAlias> {
  let cleaned = strip_comments(src);
  re()
    .captures_iter(&cleaned)
    .filter_map(|c| {
      let m = c.get(1)?;
      Some(UsingAlias {
        name:            m.as_str().to_string(),
        name_start_byte: m.start(),
        name_end_byte:   m.end(),
      })
    })
    .collect()
}

/// Find an alias with the exact given name, if any.
pub fn find<'a>(
  aliases: &'a [UsingAlias],
  name: &str,
) -> Option<&'a UsingAlias> {
  aliases.iter().find(|a| a.name == name)
}

/// A top-level declaration found by surface-text scanning (used when we don't have a
/// real index for a file, e.g. completing `OtherFile.<cursor>` for a file we haven't
/// compiled).
#[derive(Debug, Clone)]
pub struct TopLevelDecl {
  pub kind:        DeclKind,
  pub name:        String,
  /// The declaration head as written, minus the unique id and the value/body —
  /// `annotation flatten(field) :FlattenOptions`. Mirrors `NodeInfo::signature` so
  /// completion details read the same whether or not the file made it into the index.
  pub signature:   String,
  pub doc_comment: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclKind {
  Struct,
  Enum,
  Interface,
  Annotation,
  Const,
  Using,
}

/// Scan a file's source for top-level declarations of named items. This is a regex pass
/// over comment-stripped text — it doesn't try to track nesting, so nested types are
/// missed. Doc comments are gathered from contiguous `# ...` lines immediately preceding
/// each declaration.
pub fn scan_top_level(src: &str) -> Vec<TopLevelDecl> {
  use std::sync::OnceLock;
  static R: OnceLock<Regex> = OnceLock::new();
  let re = R.get_or_init(|| {
        Regex::new(r"^\s*(struct|enum|interface|annotation|const|using)\s+([A-Za-z_][A-Za-z0-9_]*)")
            .unwrap()
    });
  let lines: Vec<&str> = src.lines().collect();
  let mut out = Vec::new();
  for (i, line) in lines.iter().enumerate() {
    let Some(c) = re.captures(line) else { continue };
    let kind = match &c[1] {
      "struct" => DeclKind::Struct,
      "enum" => DeclKind::Enum,
      "interface" => DeclKind::Interface,
      "annotation" => DeclKind::Annotation,
      "const" => DeclKind::Const,
      "using" => DeclKind::Using,
      _ => continue,
    };
    let name = c[2].to_string();
    // Walk back collecting contiguous comment lines.
    let mut doc_lines: Vec<&str> = Vec::new();
    let mut j = i;
    while j > 0 {
      j -= 1;
      let prev = lines[j].trim_start();
      if let Some(rest) = prev.strip_prefix('#') {
        doc_lines.push(rest.trim_start_matches(' '));
      } else {
        break;
      }
    }
    doc_lines.reverse();
    let doc = (!doc_lines.is_empty()).then(|| doc_lines.join("\n"));
    out.push(TopLevelDecl {
      kind,
      name,
      signature: decl_signature(line),
      doc_comment: doc,
    });
  }
  out
}

/// Reduce a declaration line to its signature: drop any trailing comment, the body or
/// value (everything from `{` or the first `=`), the terminating `;`, and the `@0x…`
/// unique id, then tidy the whitespace the id left behind.
///
/// Declarations that wrap onto a second line simply yield the part on the first — a
/// truncated signature still beats no signature at all.
fn decl_signature(line: &str) -> String {
  use std::sync::OnceLock;
  static R: OnceLock<Regex> = OnceLock::new();
  let id_re = R.get_or_init(|| Regex::new(r"@0[xX][0-9a-fA-F_]+\s*").unwrap());

  let head = line.split('#').next().unwrap_or(line);
  let head = head.split('{').next().unwrap_or(head);
  let head = head.split('=').next().unwrap_or(head);
  let head = head.trim().trim_end_matches(';').trim_end();
  let stripped = id_re.replace_all(head, "");
  // `annotation foo (field)` -> `annotation foo(field)`, and collapse the runs of
  // whitespace that padded the id.
  let mut out = String::with_capacity(stripped.len());
  for word in stripped.split_whitespace() {
    if !out.is_empty() && !word.starts_with('(') {
      out.push(' ');
    }
    out.push_str(word);
  }
  out
}

/// Every `import "PATH"` in the file, in source order and deduplicated. Covers all three
/// spellings — `using X = import "..."`, `using import "...".Y` and `$import "..."` —
/// since they share the `import "…"` token.
pub fn imported_paths(src: &str) -> Vec<String> {
  use std::sync::OnceLock;
  static R: OnceLock<Regex> = OnceLock::new();
  let re = R.get_or_init(|| Regex::new(r#"\bimport\s+"([^"]+)""#).unwrap());
  let cleaned = strip_comments(src);
  let mut out: Vec<String> = Vec::new();
  for c in re.captures_iter(&cleaned) {
    let path = c[1].to_string();
    if !out.contains(&path) {
      out.push(path);
    }
  }
  out
}

/// `using NAME = import "PATH";` — return PATH for a given NAME, if any.
pub fn import_path_for(src: &str, name: &str) -> Option<String> {
  use std::sync::OnceLock;
  static R: OnceLock<Regex> = OnceLock::new();
  let re = R.get_or_init(|| {
    Regex::new(
      r#"\busing\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*import\s+"([^"]+)"\s*;"#,
    )
    .unwrap()
  });
  let cleaned = strip_comments(src);
  for c in re.captures_iter(&cleaned) {
    if &c[1] == name {
      return Some(c[2].to_string());
    }
  }
  None
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn scans_basic() {
    let src = "using UUID = Data;\nusing UTCSecondsSinceEpoch = UInt64;\n";
    let a = scan(src);
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].name, "UUID");
    assert_eq!(
      &src[a[1].name_start_byte..a[1].name_end_byte],
      "UTCSecondsSinceEpoch"
    );
  }

  #[test]
  fn ignores_comments() {
    let src = "# using Foo = Bar;\nusing Real = Text;\n";
    let a = scan(src);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].name, "Real");
  }

  #[test]
  fn imported_paths_finds_every_import_form() {
    let src = concat!(
      "using Json = import \"/capnp/compat/json.capnp\";\n",
      "using import \"sibling.capnp\".Thing;\n",
      "$import \"/capnp/c++.capnp\".namespace(\"foo\");\n",
      "# using Ignored = import \"commented.capnp\";\n",
      "using Again = import \"/capnp/compat/json.capnp\";\n",
    );
    assert_eq!(
      imported_paths(src),
      vec![
        "/capnp/compat/json.capnp",
        "sibling.capnp",
        "/capnp/c++.capnp",
      ]
    );
  }

  // The surface scanner is what feeds completion `detail` for imported files the
  // compiler pruned out of the CodeGeneratorRequest, so its signatures have to read
  // like the ones we build from the index: no unique id, no body, no value.
  #[test]
  fn scan_top_level_renders_signatures() {
    let src = concat!(
      "annotation flatten @0x82d3e852af0336bf (field, group, union) :FlattenOptions;\n",
      "annotation name @0xfa5afd9a7cbb0e2b (field, enumerant) :Text;\n",
      "struct FlattenOptions @0x40e7bbe0d0454b95 {\n",
      "const maxAge :UInt32 = 3600;  # seconds\n",
      "struct Map(Key, Value) {\n",
      "using Json = import \"/capnp/compat/json.capnp\";\n",
    );
    let sigs: Vec<String> = scan_top_level(src)
      .into_iter()
      .map(|d| d.signature)
      .collect();
    assert_eq!(
      sigs,
      vec![
        "annotation flatten(field, group, union) :FlattenOptions",
        "annotation name(field, enumerant) :Text",
        "struct FlattenOptions",
        "const maxAge :UInt32",
        "struct Map(Key, Value)",
        "using Json",
      ]
    );
  }
}
