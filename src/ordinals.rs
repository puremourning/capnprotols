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
    // Comment and string contents shouldn't influence brace nesting or ordinal scanning.
    let cleaned = strip_for_scan(text);
    let outer = enclosing_struct(&cleaned, cursor)?;
    // The struct may not be closed yet (the user is mid-edit). Scan to end-of-text in
    // that case so we still see the existing fields.
    let close = matching_close(&cleaned, outer.brace_byte).unwrap_or(cleaned.len());
    let body = &cleaned[outer.brace_byte + 1..close];
    let max = scan_max_ordinal(body);
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
/// when we pass `cursor`. Returns the topmost (innermost) `struct`-kind brace still open.
fn enclosing_struct(text: &str, cursor: usize) -> Option<OpenBrace> {
    let bytes = text.as_bytes();
    let cursor = cursor.min(bytes.len());
    let mut stack: Vec<OpenBrace> = Vec::new();
    let mut i = 0;
    while i < cursor {
        let b = bytes[i];
        if b == b'{' {
            // Determine what opened this brace by looking at the preceding token sequence.
            stack.push(OpenBrace {
                brace_byte: i,
                kind: classify_block(text, i),
            });
        } else if b == b'}' {
            stack.pop();
        }
        i += 1;
    }
    // Innermost struct is the topmost Struct in the stack.
    stack
        .into_iter()
        .rev()
        .find(|f| f.kind == BlockKind::Struct)
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

/// Largest `@<n>` ordinal in `body`, ignoring any inside *nested* `struct ... {}` blocks
/// (whose ordinals are scoped separately). Groups and unions are walked through normally.
fn scan_max_ordinal(body: &str) -> Option<u32> {
    let bytes = body.as_bytes();
    let mut max: Option<u32> = None;
    let mut i = 0;
    while i < bytes.len() {
        // Skip nested struct blocks entirely.
        if bytes[i] == b'{' {
            // We are inside the parent struct's body; any { encountered opens a sub-block.
            // If it opens a nested struct, skip past its matching }. Otherwise descend.
            if classify_block(body, i) == BlockKind::Struct {
                if let Some(close) = matching_close(body, i) {
                    i = close + 1;
                    continue;
                } else {
                    break;
                }
            }
            // Group/union/other — descend (counted by the bare iteration).
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
    fn ignores_at_in_string() {
        let src = "struct A { foo @0 :Text = \"hello @99 world\"; bar @|";
        assert_eq!(ord_at(src), Some(1));
    }
}
