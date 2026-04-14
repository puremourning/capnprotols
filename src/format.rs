//! Cap'n Proto schema formatter.
//!
//! The strategy is intentionally conservative: we walk the tree-sitter parse, collect
//! leaf tokens, and decide the whitespace between each adjacent pair from a small ruleset
//! keyed on the token kinds plus the current brace depth. Token interiors are copied
//! verbatim, so identifiers / string literals / comment contents are never touched.
//!
//! Things we DO touch:
//! - indentation (computed from `{...}` nesting depth, 2 spaces per level),
//! - spacing around `:` (one space before, none after — Kenton's `id @0 :Id;` form),
//! - spacing around `=`, `,`, `(`, `)`, `.`, `;`, `$`, `[]`, `->`,
//! - blank-line conventions (one between top-level decls; collapse multi-blank runs),
//! - trailing whitespace (stripped on every line),
//! - final newline (always one).
//!
//! Things we DON'T touch in v1:
//! - the contents of comment paragraphs (no reflow / wrap / bullet detection),
//! - the contents of doc-comment paragraphs that exceed `max_width` (left as-is),
//! - alignment of `@N` ordinals,
//! - the contents of `# capnpfmt: off` / `# capnpfmt: on` regions (stage 6).
//!
//! Bails (returns `None`) on any parse error so we never reformat broken input.

use crate::config::FormatOptions;

const INDENT_UNIT: usize = 2;

/// One byte range in the original text that exceeded `max_width` after formatting and
/// couldn't be auto-wrapped. The server publishes these as Diagnostics so the user
/// notices them.
#[derive(Debug, Clone)]
pub struct LongLineWarning {
    /// 0-based line in the *formatted* output.
    pub line: u32,
    /// Width (in chars) of the offending line.
    pub width: u32,
}

/// Formatter result: the new text plus any unwrappable long-line warnings.
#[derive(Debug)]
pub struct FormatOutput {
    pub text: String,
    pub warnings: Vec<LongLineWarning>,
}

/// Formatter entry point. Returns `None` if the buffer can't be safely formatted
/// (parse errors). When the input is already canonical, the returned `text` equals the
/// input — callers should compare to decide whether to emit a `TextEdit`.
pub fn format_document(text: &str, opts: &FormatOptions) -> Option<FormatOutput> {
    let tree = parse(text)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }

    let tokens = collect_leaves(root, text);
    if tokens.is_empty() {
        return Some(FormatOutput {
            text: text.to_string(),
            warnings: Vec::new(),
        });
    }
    let verbatim_regions = collect_verbatim_regions(&tokens);
    let normalised = format_with_verbatim(text, &tokens, &verbatim_regions)?;
    let (with_wraps, warnings) = enforce_width(&normalised, opts);
    Some(FormatOutput {
        text: with_wraps,
        warnings,
    })
}

/// Byte ranges in the source between matching `# capnpfmt: off` and `# capnpfmt: on`
/// comments — those bytes pass through unchanged. The marker comments themselves are
/// included in the verbatim range (so the user sees them in the output).
fn collect_verbatim_regions(tokens: &[Tok]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut open: Option<usize> = None;
    for tok in tokens {
        if tok.kind != "comment" {
            continue;
        }
        let body = tok.text.trim_start_matches('#').trim();
        if body == "capnpfmt: off" && open.is_none() {
            open = Some(tok.byte_start);
        } else if body == "capnpfmt: on" {
            if let Some(start) = open.take() {
                out.push((start, tok.byte_end));
            }
        }
    }
    out
}

fn in_verbatim(regions: &[(usize, usize)], byte: usize) -> bool {
    regions.iter().any(|&(s, e)| byte >= s && byte < e)
}

fn format_with_verbatim(
    text: &str,
    tokens: &[Tok],
    verbatim: &[(usize, usize)],
) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut depth: i32 = 0;
    let mut paren_depth: i32 = 0;
    let mut just_emitted_newline = true;

    let mut i = 0;
    while i < tokens.len() {
        let tok = &tokens[i];

        // If this token is inside a verbatim region, copy the entire span (including any
        // intervening whitespace from the source) straight through, then advance past
        // every token contained in the region.
        if let Some(&(vstart, vend)) = verbatim
            .iter()
            .find(|&&(s, e)| tok.byte_start >= s && tok.byte_start < e)
        {
            // Ensure we're at a line boundary before pasting the verbatim block.
            if !just_emitted_newline {
                rstrip_line(&mut out);
                out.push('\n');
                just_emitted_newline = true;
            }
            out.push_str(&text[vstart..vend]);
            // Skip every token that ends inside this region.
            while i < tokens.len() && tokens[i].byte_end <= vend {
                let kind = tokens[i].text.as_str();
                if kind == "{" {
                    depth += 1;
                } else if kind == "}" {
                    depth -= 1;
                }
                i += 1;
            }
            // The verbatim block contains its own newlines; reset state accordingly.
            just_emitted_newline = out.ends_with('\n');
            continue;
        }

        let prev = if i > 0 { Some(&tokens[i - 1]) } else { None };
        let depth_for_this = match tok.text.as_str() {
            "}" => (depth - 1).max(0),
            _ => depth,
        };

        let sep = separator(prev, tok, depth_for_this as usize, paren_depth);
        match sep {
            Sep::None => {}
            Sep::Space => {
                if !just_emitted_newline {
                    out.push(' ');
                }
            }
            Sep::Newline { blank, indent } => {
                if !just_emitted_newline {
                    rstrip_line(&mut out);
                    out.push('\n');
                }
                if blank {
                    out.push('\n');
                }
                for _ in 0..indent {
                    out.push(' ');
                }
            }
        }

        out.push_str(&tok.text);
        just_emitted_newline = false;

        match tok.text.as_str() {
            "{" => depth += 1,
            "}" => depth -= 1,
            "(" | "[" => paren_depth += 1,
            ")" | "]" => paren_depth -= 1,
            _ => {}
        }
        i += 1;
    }

    rstrip_line(&mut out);
    if !out.ends_with('\n') {
        out.push('\n');
    }

    Some(out)
}

#[derive(Debug)]
struct Tok {
    /// The leaf node's `kind()`. For anonymous tokens this equals the literal text
    /// (e.g. `";"`); for named atomic nodes it's the kind name (`"comment"`,
    /// `"field_identifier"`, …).
    kind: String,
    /// Raw source text covered by the leaf, copied verbatim.
    text: String,
    /// Source byte range of the leaf — used to identify `# capnpfmt: off` regions.
    byte_start: usize,
    byte_end: usize,
    /// Whether the token sits at the very start of its source line (no non-whitespace
    /// chars before it on that line). Used to preserve "comment is on its own line".
    starts_line: bool,
    /// True if the source had at least one blank line immediately before this token —
    /// i.e. between the prior token and this one, two or more `\n` separated only by
    /// whitespace. Lets us preserve user-intentional vertical grouping.
    blank_line_before: bool,
}

fn collect_leaves(root: tree_sitter::Node<'_>, src: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    walk(root, src, bytes, &mut out);
    out
}

fn walk(node: tree_sitter::Node<'_>, src: &str, bytes: &[u8], out: &mut Vec<Tok>) {
    // Treat any quoted/text-bearing wrapper as atomic — descending into them produces
    // separate `"` / `string_fragment` / `escape_sequence` leaves, and inserting
    // whitespace between those would corrupt the literal.
    if matches!(
        node.kind(),
        "string" | "concatenated_string" | "import_path" | "data_string" | "block_text"
    ) {
        let range = node.byte_range();
        let text = src[range.clone()].to_string();
        let starts_line = is_line_start(bytes, range.start);
        let blank_line_before = blank_line_before(bytes, range.start);
        out.push(Tok {
            kind: node.kind().to_string(),
            text,
            byte_start: range.start,
            byte_end: range.end,
            starts_line,
            blank_line_before,
        });
        return;
    }
    if node.child_count() == 0 {
        let range = node.byte_range();
        // Skip zero-length tokens (tree-sitter sometimes emits sentinel ERROR nodes; we
        // already bailed on those, but be defensive).
        if range.start >= range.end {
            return;
        }
        let text = src[range.clone()].to_string();
        let starts_line = is_line_start(bytes, range.start);
        let blank_line_before = blank_line_before(bytes, range.start);
        out.push(Tok {
            kind: node.kind().to_string(),
            text,
            byte_start: range.start,
            byte_end: range.end,
            starts_line,
            blank_line_before,
        });
        return;
    }
    let mut cur = node.walk();
    for c in node.children(&mut cur) {
        walk(c, src, bytes, out);
    }
}

/// True if the source between the prior non-whitespace byte and `pos` contains 2+
/// newlines. Indicates the user explicitly left a blank line.
fn blank_line_before(bytes: &[u8], pos: usize) -> bool {
    let mut newlines = 0;
    let mut i = pos;
    while i > 0 {
        match bytes[i - 1] {
            b'\n' => {
                newlines += 1;
                if newlines >= 2 {
                    return true;
                }
            }
            b' ' | b'\t' | b'\r' => {}
            _ => return false,
        }
        i -= 1;
    }
    false
}

fn is_line_start(bytes: &[u8], pos: usize) -> bool {
    let mut i = pos;
    while i > 0 {
        match bytes[i - 1] {
            b'\n' => return true,
            b' ' | b'\t' => i -= 1,
            _ => return false,
        }
    }
    true
}

#[derive(Debug, Clone, Copy)]
enum Sep {
    None,
    Space,
    Newline { blank: bool, indent: usize },
}

/// Decide what whitespace separates `tok` from `prev`. `depth` is the indent depth that
/// applies to `tok` (closing `}` already has depth-1 here). `paren_depth` is the number
/// of open `(`/`[` we're inside, which affects spacing around `=` (kwarg form vs
/// statement-level assignment).
fn separator(prev: Option<&Tok>, tok: &Tok, depth: usize, paren_depth: i32) -> Sep {
    let prev = match prev {
        None => return Sep::Newline { blank: false, indent: depth * INDENT_UNIT },
        Some(p) => p,
    };
    let p = prev.text.as_str();
    let t = tok.text.as_str();

    // === Highest-priority rules: things that override generic punctuation handling ===

    // Comments preserve their own-line vs trailing association, and their blank-line
    // separation from the previous token.
    if tok.kind == "comment" {
        if tok.starts_line {
            return Sep::Newline {
                blank: tok.blank_line_before,
                indent: depth * INDENT_UNIT,
            };
        }
        return Sep::Space;
    }
    if prev.kind == "comment" {
        return Sep::Newline {
            blank: tok.blank_line_before,
            indent: depth * INDENT_UNIT,
        };
    }

    // `;`, `,`, `)`, `]` never have a space before them.
    if matches!(t, ";" | "," | ")" | "]") {
        return Sep::None;
    }

    // After `;`, `{`, `}` we always start a new line. Insert a blank line iff the user
    // had one in the source — we don't auto-insert blanks (that decision is theirs).
    if matches!(p, ";" | "{" | "}") {
        return Sep::Newline {
            blank: tok.blank_line_before,
            indent: depth * INDENT_UNIT,
        };
    }

    // === Operator and punctuation spacing ===

    // `:` — space before, none after (`name @N :Type`).
    if t == ":" {
        return Sep::Space;
    }
    if p == ":" {
        return Sep::None;
    }

    // `=` — outside parens this is `using/const/default = …`, with one space each side.
    // Inside parens it's the kwarg `name=value` form, no spaces (Kenton + user style).
    if t == "=" || p == "=" {
        return if paren_depth > 0 { Sep::None } else { Sep::Space };
    }

    // `.` and `(`, `[` — no space around (no space before `(` for calls/generics either).
    if matches!(t, "(" | "[" | ".") {
        return Sep::None;
    }
    if matches!(p, "(" | "[" | ".") {
        return Sep::None;
    }

    // After `,` — one space.
    if p == "," {
        return Sep::Space;
    }

    // `$Annotation` — no space between `$` and the following ident; space before `$`
    // (handled by default rule).
    if p == "$" {
        return Sep::None;
    }

    // `->` — space on both sides.
    if t == "->" || p == "->" {
        return Sep::Space;
    }

    // Default: single space.
    Sep::Space
}

/// True if `t` looks like the first token of a top-level declaration. Used to insert a
/// blank line between successive top-level decls.
fn is_decl_start(t: &str) -> bool {
    matches!(
        t,
        "struct" | "enum" | "interface" | "const" | "annotation" | "using" | "namespace" | "$"
    )
}

fn rstrip_line(out: &mut String) {
    while out.ends_with(' ') || out.ends_with('\t') {
        out.pop();
    }
}

/// Per-line width pass. Walks the formatted output line-by-line; when a line exceeds
/// `max_width`, attempts the wrappers in order:
///   1. trailing-comment relocation (`field;  # ...` -> push the `#` onto its own line),
///   2. break a long annotation chain at each `$Annotation(...)`,
///   3. break a long generic argument list inside its parens.
/// If none of those fit (or the line still exceeds the limit afterwards) and
/// `warn_long_lines` is on, a `LongLineWarning` is returned. The returned text uses the
/// (possibly wrapped) lines.
fn enforce_width(text: &str, opts: &FormatOptions) -> (String, Vec<LongLineWarning>) {
    let max = opts.max_width as usize;
    let mut warnings = Vec::new();
    let mut out_lines: Vec<String> = Vec::new();

    for raw_line in text.split('\n') {
        let line = raw_line.to_string();
        if line.chars().count() <= max {
            out_lines.push(line);
            continue;
        }
        // Always try trailing-comment relocation first (it strictly improves things by
        // peeling the comment off the code). Then, if the code line is still over the
        // limit, try the code-shape wrappers on it.
        let mut chunks: Vec<String> = if let Some(split) = wrap_trailing_comment(&line, max) {
            split
        } else {
            vec![line]
        };
        if chunks[0].chars().count() > max {
            let code = chunks[0].clone();
            let wrapped = wrap_annotation_chain(&code, max)
                .filter(|w| w.iter().all(|l| l.chars().count() <= max))
                .or_else(|| {
                    wrap_generic_args(&code, max)
                        .filter(|w| w.iter().all(|l| l.chars().count() <= max))
                });
            if let Some(w) = wrapped {
                let tail: Vec<String> = chunks.split_off(1);
                chunks = w.into_iter().chain(tail.into_iter()).collect();
            }
        }
        for l in &chunks {
            if l.chars().count() > max && opts.warn_long_lines {
                warnings.push(LongLineWarning {
                    line: (out_lines.len() + chunks.iter().position(|x| x == l).unwrap_or(0)) as u32,
                    width: l.chars().count() as u32,
                });
            }
        }
        out_lines.extend(chunks);
    }

    (out_lines.join("\n"), warnings)
}

/// `    field @0 :Text;  # ...long comment...` -> two lines, comment on its own line at
/// the field's indent. Returns `Some(lines)` only if the line *has* a trailing inline
/// comment.
fn wrap_trailing_comment(line: &str, _max: usize) -> Option<Vec<String>> {
    let (code, comment) = split_trailing_comment(line)?;
    let indent = leading_indent(&code);
    let trimmed_code = code.trim_end();
    Some(vec![
        trimmed_code.to_string(),
        format!("{indent}{comment}"),
    ])
}

/// Try to break a long field declaration at each `$Annotation(...)`. The first line is
/// `<indent>field @N :Type` (no annotations), each subsequent line is
/// `<indent + 4>$Annotation(...)`. Returns None if the line doesn't look like a field
/// with annotations.
fn wrap_annotation_chain(line: &str, _max: usize) -> Option<Vec<String>> {
    let bytes = line.as_bytes();
    // Find each `$` that starts an annotation at top level (paren depth 0).
    let mut depth: i32 = 0;
    let mut splits: Vec<usize> = Vec::new();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'$' if depth == 0 => splits.push(i),
            _ => {}
        }
    }
    if splits.is_empty() {
        return None;
    }
    let indent = leading_indent(line);
    let inner_indent = format!("{indent}    ");
    let head = line[..splits[0]].trim_end().to_string();
    let mut out = vec![head];
    for w in 0..splits.len() {
        let start = splits[w];
        let end = splits.get(w + 1).copied().unwrap_or(line.len());
        let chunk = line[start..end].trim_end().to_string();
        out.push(format!("{inner_indent}{chunk}"));
    }
    Some(out)
}

/// Break a long generic instantiation inside the outermost `(...)`:
///   `field @0 :List(VeryLong, Other);`
/// becomes
///   ```
///   field @0 :List(
///     VeryLong,
///     Other);
///   ```
/// (with continuation indent matching the column of the `(`). Returns None if there's
/// no outer paren on the line.
fn wrap_generic_args(line: &str, _max: usize) -> Option<Vec<String>> {
    let bytes = line.as_bytes();
    let open = bytes.iter().position(|&b| b == b'(')?;
    let mut depth: i32 = 0;
    let mut close: Option<usize> = None;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let inner = &line[open + 1..close];
    if !inner.contains(',') {
        // Single-arg generics don't gain anything from the break.
        return None;
    }
    // Split args at top-level commas only.
    let mut args: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut d: i32 = 0;
    for c in inner.chars() {
        match c {
            '(' | '[' | '{' => {
                d += 1;
                buf.push(c);
            }
            ')' | ']' | '}' => {
                d -= 1;
                buf.push(c);
            }
            ',' if d == 0 => {
                args.push(buf.trim().to_string());
                buf.clear();
            }
            _ => buf.push(c),
        }
    }
    let last = buf.trim().to_string();
    if !last.is_empty() {
        args.push(last);
    }
    if args.len() < 2 {
        return None;
    }

    let head = &line[..=open]; // up to and including '('
    let tail = &line[close..]; // ')...rest of line'
    let indent = leading_indent(line);
    let arg_indent = format!("{indent}    ");
    let mut out = vec![head.trim_end().to_string()];
    for (i, arg) in args.iter().enumerate() {
        let suffix = if i + 1 < args.len() { "," } else { "" };
        out.push(format!("{arg_indent}{arg}{suffix}"));
    }
    out.push(format!("{indent}{tail}"));
    Some(out)
}

/// Returns `(code_part, "# comment...")` if the line has a trailing inline comment that
/// isn't the only non-whitespace thing on the line.
fn split_trailing_comment(line: &str) -> Option<(String, String)> {
    let bytes = line.as_bytes();
    // Find a `#` outside string literals (capnp strings are double-quoted).
    let mut in_string = false;
    let mut hash: Option<usize> = None;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => in_string = !in_string,
            b'\\' if in_string => {
                i += 2;
                continue;
            }
            b'#' if !in_string => {
                hash = Some(i);
                break;
            }
            _ => {}
        }
        i += 1;
    }
    let h = hash?;
    let before = &line[..h];
    if before.trim().is_empty() {
        // Whole-line comment, not a trailing one.
        return None;
    }
    Some((before.to_string(), line[h..].to_string()))
}

fn leading_indent(line: &str) -> String {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}

fn parse(text: &str) -> Option<tree_sitter::Tree> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(tree_sitter_capnp::language()).ok()?;
    parser.parse(text, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(input: &str) -> Option<String> {
        format_document(input, &FormatOptions::default()).map(|o| o.text)
    }

    #[test]
    fn parse_errors_skip_format() {
        let src = "@0xeaf06436acd04fcd;\nstruct A { BROKEN_TOKEN!!! }\n";
        assert_eq!(fmt(src), None);
    }

    #[test]
    fn final_newline_added() {
        let src = "@0xeaf06436acd04fce;\nstruct A {\n  foo @0 :Text;\n}";
        let out = fmt(src).expect("formatted");
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn trailing_whitespace_stripped() {
        let src = "@0xeaf06436acd04fcf;   \nstruct A {\n  foo @0 :Text;   \n}\n";
        let out = fmt(src).expect("formatted");
        for (i, line) in out.lines().enumerate() {
            assert!(
                !line.ends_with(' ') && !line.ends_with('\t'),
                "line {} has trailing ws: {:?}", i + 1, line
            );
        }
    }

    #[test]
    fn colon_spacing_normalised() {
        // All three of these should normalise to `foo @0 :Text;`.
        let inputs = [
            "@0xeaf06436acd04fd0;\nstruct A { foo @0:Text; }\n",
            "@0xeaf06436acd04fd0;\nstruct A { foo @0 : Text; }\n",
            "@0xeaf06436acd04fd0;\nstruct A { foo @0 :  Text; }\n",
        ];
        for input in inputs {
            let out = fmt(input).expect("formatted");
            assert!(
                out.contains("foo @0 :Text;"),
                "got {out:?} for input {input:?}"
            );
        }
    }

    #[test]
    fn bad_indent_normalised() {
        let src = "@0xeaf06436acd04fd1;\nstruct A {\n        foo @0 :Text;\n}\n";
        let out = fmt(src).expect("formatted");
        // Field indented 2 spaces, not 8.
        assert!(out.contains("\n  foo @0 :Text;\n"), "got: {out}");
    }

    #[test]
    fn user_blank_lines_preserved() {
        let src = "@0xeaf06436acd04fd2;\nstruct A { foo @0 :Text; }\n\nstruct B { bar @0 :Text; }\n";
        let out = fmt(src).expect("formatted");
        assert!(out.contains("}\n\nstruct B"), "user blank line not preserved:\n{out}");
    }

    #[test]
    fn no_auto_blank_between_consecutive_decls() {
        // Two top-level decls on consecutive lines (no user blank): formatter must NOT
        // invent a blank.
        let src = "@0xeaf06436acd04fd2;\nstruct A { foo @0 :Text; }\nstruct B { bar @0 :Text; }\n";
        let out = fmt(src).expect("formatted");
        assert!(!out.contains("}\n\nstruct B"), "auto-inserted blank where source had none:\n{out}");
        assert!(out.contains("}\nstruct B"), "structs not consecutive:\n{out}");
    }

    #[test]
    fn consecutive_using_imports_stay_tight() {
        // Reported by user: two `using = import` declarations on the same line ended up
        // with a blank between them after formatting. Formatter must place them on
        // consecutive lines with no blank.
        let src = "@0xeaf06436acd04fdb;\nusing Types = import \"types.capnp\";using Json = import \"/capnp/compat/json.capnp\";\n";
        let out = fmt(src).expect("formatted");
        assert!(
            out.contains("using Types = import \"types.capnp\";\nusing Json = import \"/capnp/compat/json.capnp\";\n"),
            "consecutive imports not tight:\n{out}"
        );
    }

    #[test]
    fn import_path_is_atomic() {
        // Reported by user: import_path's contents were getting padded with spaces.
        let src = "@0xeaf06436acd04fdc;\nusing X = import \"/capnp/compat/json.capnp\";\n";
        let out = fmt(src).expect("formatted");
        assert!(out.contains("import \"/capnp/compat/json.capnp\";"), "import path corrupted:\n{out}");
    }

    #[test]
    fn long_annotation_chain_breaks_at_dollars() {
        let src = "@0xeaf06436acd04fd7;\nstruct A {\n  myReallyQuiteLongFieldName @0 :Text $Json.name(\"a_long_external_name\") $Anno.other(value=\"x\");\n}\n";
        // Use a tighter max so the chain definitely needs wrapping.
        let opts = FormatOptions { max_width: 80, ..FormatOptions::default() };
        let out = format_document(src, &opts).expect("formatted");
        assert!(out.text.contains("\n      $Json.name(\"a_long_external_name\")\n"), "annotation 1 not wrapped:\n{}", out.text);
        assert!(out.text.contains("\n      $Anno.other(value=\"x\");\n"), "annotation 2 not wrapped:\n{}", out.text);
        assert!(out.warnings.is_empty(), "unexpected warnings: {:?}", out.warnings);
    }

    #[test]
    fn long_generic_args_break_inside_parens() {
        // Multi-arg generics require user-defined generic types — capnp's `List` is
        // single-arg. Use a 2-param user struct as the receiver.
        let src = "@0xeaf06436acd04fd8;\nstruct Pair(K, V) { key @0 :K; value @1 :V; }\nstruct A {\n  m @0 :Pair(SomeReallyQuiteLongType, AnotherSomewhatLongType);\n}\n";
        let opts = FormatOptions { max_width: 60, ..FormatOptions::default() };
        let out = format_document(src, &opts).expect("formatted");
        assert!(out.text.contains("Pair(\n"), "Pair not opened on its own line:\n{}", out.text);
        assert!(out.text.contains("    SomeReallyQuiteLongType,\n"), "first arg:\n{}", out.text);
        assert!(out.text.contains("    AnotherSomewhatLongType"), "last arg:\n{}", out.text);
    }

    #[test]
    fn unwrappable_long_line_emits_warning() {
        // A long line we can't break: a single field with a really long type name.
        let src = "@0xeaf06436acd04fd9;\nstruct A {\n  field @0 :SomeAbsolutelyEnormousTypeNameThatHasNoNaturalBreakPointAvailable;\n}\n";
        let opts = FormatOptions { max_width: 40, ..FormatOptions::default() };
        let out = format_document(src, &opts).expect("formatted");
        assert!(!out.warnings.is_empty(), "expected at least one warning");
    }

    #[test]
    fn inline_trailing_comment_moves_to_next_line_when_over_width() {
        let src = "@0xeaf06436acd04fda;\nstruct A {\n  foo @0 :Text;  # this is a long inline comment that pushes the line over the limit\n}\n";
        let opts = FormatOptions { max_width: 60, ..FormatOptions::default() };
        let out = format_document(src, &opts).expect("formatted");
        // Field stays on one line; comment got pushed to next line at the field's indent.
        assert!(out.text.contains("\n  foo @0 :Text;\n  # this is a long inline comment"), "comment not relocated:\n{}", out.text);
    }

    #[test]
    fn capnpfmt_off_block_is_preserved_verbatim() {
        let src = "@0xeaf06436acd04fd6;\nstruct A {\n# capnpfmt: off\n           foo @0:Text;     # this stays ugly\n     bar @1:UInt8;\n# capnpfmt: on\n  baz @2 :Bool;\n}\n";
        let out = fmt(src).expect("formatted");
        // The off-region content survives unchanged.
        assert!(out.contains("           foo @0:Text;     # this stays ugly\n"), "lost ugly foo:\n{out}");
        assert!(out.contains("     bar @1:UInt8;\n"), "lost ugly bar:\n{out}");
        // Outside the off-region, normal formatting applies.
        assert!(out.contains("\n  baz @2 :Bool;\n"), "baz wasn't normalised:\n{out}");
    }

    #[test]
    fn comment_block_reindents_with_declaration() {
        let src = "@0xeaf06436acd04fd3;\nstruct A {\n        foo @0 :Text;\n        # doc line one\n        # doc line two\n        bar @1 :UInt8;\n}\n";
        let out = fmt(src).expect("formatted");
        assert!(out.contains("\n  foo @0 :Text;\n"), "field reindented?\n{out}");
        assert!(out.contains("\n  # doc line one\n"), "comment reindented?\n{out}");
        assert!(out.contains("\n  # doc line two\n"), "comment reindented?\n{out}");
    }
}
