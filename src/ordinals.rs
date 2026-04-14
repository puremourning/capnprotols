//! Compute the next valid `@<n>` ordinal for a field declaration in a struct.
//!
//! Cap'n Proto requires field ordinals to be contiguous starting from zero, scoped to the
//! enclosing struct. Groups and unions share the parent struct's ID space; nested structs
//! get their own. We work entirely from the buffer text (with comments/strings stripped)
//! so this stays usable on broken-but-parseable buffers where the compiler may not have
//! produced a CGR.
//!
//! The hot path is short and bounded by the size of the enclosing struct, so a textual
//! scan is plenty.

use crate::aliases::strip_comments;

/// Block-opening keyword found by walking back from the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Struct,
    Enum,
    Group,
    Union,
    Other,
}

#[derive(Debug)]
struct OpenBrace {
    /// Byte index of the `{` itself.
    brace_byte: usize,
    kind: BlockKind,
}

/// Compute the next ordinal to suggest at `cursor`. Returns `None` if the cursor isn't
/// inside a struct.
pub fn next_ordinal_at(text: &str, cursor: usize) -> Option<u32> {
    let cleaned = strip_for_scan(text);
    let outer = enclosing_struct_or_enum(&cleaned, cursor)?;
    // The block may not be closed yet (the user is mid-edit). Scan to end-of-text in
    // that case so we still see the existing fields/enumerants.
    let close = matching_close(&cleaned, outer.brace_byte).unwrap_or(cleaned.len());
    let body = &cleaned[outer.brace_byte + 1..close];
    let max = scan_max_ordinal(body, outer.kind);
    Some(max.map_or(0, |m| m + 1))
}

/// Strip both `# ...` line comments and `"..."` string literals — replacing their bytes
/// with spaces so byte offsets are preserved.
fn strip_for_scan(src: &str) -> String {
    let mut s = strip_comments(src).into_bytes();
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'"' {
            s[i] = b' ';
            i += 1;
            while i < s.len() && s[i] != b'"' && s[i] != b'\n' {
                s[i] = b' ';
                i += 1;
            }
            if i < s.len() && s[i] == b'"' {
                s[i] = b' ';
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    String::from_utf8(s).unwrap_or_default()
}

/// Walk forward, building a stack of open braces with the kind of block they open. Stop
/// when we pass `cursor`. Returns the topmost (innermost) struct-or-enum brace still open.
fn enclosing_struct_or_enum(text: &str, cursor: usize) -> Option<OpenBrace> {
    let bytes = text.as_bytes();
    let cursor = cursor.min(bytes.len());
    let mut stack: Vec<OpenBrace> = Vec::new();
    let mut i = 0;
    while i < cursor {
        let b = bytes[i];
        if b == b'{' {
            stack.push(OpenBrace {
                brace_byte: i,
                kind: classify_block(text, i),
            });
        } else if b == b'}' {
            stack.pop();
        }
        i += 1;
    }
    stack
        .into_iter()
        .rev()
        .find(|f| matches!(f.kind, BlockKind::Struct | BlockKind::Enum))
}

/// Classify what kind of block an opening `{` belongs to by scanning the tokens that
/// precede it on the same statement (back to the previous `{`, `}`, or `;`).
fn classify_block(text: &str, brace_byte: usize) -> BlockKind {
    let bytes = text.as_bytes();
    let mut i = brace_byte;
    // Stop when we hit a statement terminator or another brace.
    while i > 0 {
        i -= 1;
        match bytes[i] {
            b';' | b'{' | b'}' => {
                i += 1;
                break;
            }
            _ => {}
        }
    }
    let segment = &text[i..brace_byte];
    // `union {` or `union $Foo.bar {` (annotations between `union` and `{`)
    // `group { ... }` is unusual — capnp's `:group {` is the common form.
    // `struct Name { ... }` or `struct Name $Foo {` etc.
    let words: Vec<&str> = segment
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .collect();
    // Anonymous union has just `union` on its own; named union doesn't exist. `:group` is
    // a field type whose body is a brace-delimited declaration.
    if words.iter().any(|w| *w == "struct") {
        BlockKind::Struct
    } else if words.iter().any(|w| *w == "enum") {
        BlockKind::Enum
    } else if words.iter().any(|w| *w == "union") {
        BlockKind::Union
    } else if words.iter().any(|w| *w == "group") {
        BlockKind::Group
    } else {
        BlockKind::Other
    }
}

/// Match a `{` at `open_byte` to its closing `}`. Returns the byte index of the `}`.
fn matching_close(text: &str, open_byte: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.get(open_byte) != Some(&b'{') {
        return None;
    }
    let mut depth: i32 = 0;
    let mut i = open_byte;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Largest `@<n>` ordinal in `body`, scoped to the enclosing block's own ID space.
/// For a struct: groups/unions share the parent's ID space (so we descend through them),
/// but nested `struct { ... }` and `enum { ... }` each open their own space and are skipped.
/// For an enum: every nested block opens a different space, so all of them are skipped.
fn scan_max_ordinal(body: &str, outer: BlockKind) -> Option<u32> {
    let bytes = body.as_bytes();
    let mut max: Option<u32> = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            let kind = classify_block(body, i);
            let crosses_scope = match outer {
                BlockKind::Struct => matches!(kind, BlockKind::Struct | BlockKind::Enum),
                BlockKind::Enum => true,
                _ => false,
            };
            if crosses_scope {
                if let Some(close) = matching_close(body, i) {
                    i = close + 1;
                    continue;
                } else {
                    break;
                }
            }
        }
        if bytes[i] == b'@' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 {
                if let Ok(n) = body[i + 1..j].parse::<u32>() {
                    max = Some(max.map_or(n, |m| m.max(n)));
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    max
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ord_at(src: &str) -> Option<u32> {
        let cursor = src.find('|').expect("test source needs a `|` cursor marker");
        let stripped = src.replace('|', "");
        next_ordinal_at(&stripped, cursor)
    }

    #[test]
    fn empty_struct() {
        assert_eq!(ord_at("struct A { foo @|"), Some(0));
    }

    #[test]
    fn after_existing_fields() {
        let src = "struct A {\n  foo @0 :Text;\n  bar @1 :UInt8;\n  baz @|";
        assert_eq!(ord_at(src), Some(2));
    }

    #[test]
    fn group_shares_parent_id_space() {
        let src = "struct A {\n  foo @0 :Text;\n  inner :group {\n    a @1 :UInt8;\n    b @|";
        assert_eq!(ord_at(src), Some(2));
    }

    #[test]
    fn union_shares_parent_id_space() {
        let src = "struct A {\n  foo @0 :Text;\n  body :union {\n    a @1 :UInt8;\n    b @|";
        assert_eq!(ord_at(src), Some(2));
    }

    #[test]
    fn nested_struct_has_own_id_space() {
        let src = "struct Outer {\n  foo @0 :Text;\n  bar @5 :Int32;\n  struct Inner {\n    a @0 :Bool;\n    b @|";
        // Cursor is inside Inner -> next is 1.
        assert_eq!(ord_at(src), Some(1));
    }

    #[test]
    fn outer_ignores_nested_struct_ordinals() {
        let src = "struct Outer {\n  struct Inner { a @9 :Bool; }\n  foo @0 :Text;\n  bar @|";
        assert_eq!(ord_at(src), Some(1));
    }

    #[test]
    fn outside_struct_returns_none() {
        assert_eq!(ord_at("@|"), None);
    }

    #[test]
    fn enum_ordinals() {
        assert_eq!(ord_at("enum Side { buy @0; sell @|"), Some(1));
    }

    #[test]
    fn empty_enum() {
        assert_eq!(ord_at("enum E { first @|"), Some(0));
    }

    #[test]
    fn enum_inside_struct_has_own_space() {
        let src = "struct S {\n  foo @0 :Text;\n  bar @1 :UInt8;\n  enum Kind {\n    a @0;\n    b @|";
        assert_eq!(ord_at(src), Some(1));
    }

    #[test]
    fn struct_ignores_nested_enum_ordinals() {
        let src = "struct S {\n  enum K { a @0; b @1; }\n  foo @0 :Text;\n  bar @|";
        assert_eq!(ord_at(src), Some(1));
    }

    #[test]
    fn ignores_at_in_string() {
        let src = "struct A { foo @0 :Text = \"hello @99 world\"; bar @|";
        assert_eq!(ord_at(src), Some(1));
    }
}
