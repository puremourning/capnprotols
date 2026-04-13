//! Lightweight scanner for `using NAME = TYPE;` declarations. Cap'n Proto's compiler
//! resolves `using` aliases away in the CodeGeneratorRequest, so go-to-definition on an
//! alias name needs to consult the source text directly.

use std::sync::OnceLock;

use regex::Regex;

#[derive(Debug, Clone)]
pub struct UsingAlias {
    pub name: String,
    pub name_start_byte: usize,
    pub name_end_byte: usize,
}

fn re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // `using NAME = ...;` — capnp also allows `using import "..."`, which doesn't bind a
    // single name in the same way; we skip that form. Comments are stripped naively below.
    R.get_or_init(|| Regex::new(r"\busing\s+([A-Za-z_][A-Za-z0-9_]*)\s*=").unwrap())
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
                name: m.as_str().to_string(),
                name_start_byte: m.start(),
                name_end_byte: m.end(),
            })
        })
        .collect()
}

/// Find an alias with the exact given name, if any.
pub fn find<'a>(aliases: &'a [UsingAlias], name: &str) -> Option<&'a UsingAlias> {
    aliases.iter().find(|a| a.name == name)
}

/// A top-level declaration found by surface-text scanning (used when we don't have a
/// real index for a file, e.g. completing `OtherFile.<cursor>` for a file we haven't
/// compiled).
#[derive(Debug, Clone)]
pub struct TopLevelDecl {
    pub kind: DeclKind,
    pub name: String,
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
            } else if prev.is_empty() {
                break;
            } else {
                break;
            }
        }
        doc_lines.reverse();
        let doc = (!doc_lines.is_empty()).then(|| doc_lines.join("\n"));
        out.push(TopLevelDecl {
            kind,
            name,
            doc_comment: doc,
        });
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
        assert_eq!(&src[a[1].name_start_byte..a[1].name_end_byte], "UTCSecondsSinceEpoch");
    }

    #[test]
    fn ignores_comments() {
        let src = "# using Foo = Bar;\nusing Real = Text;\n";
        let a = scan(src);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].name, "Real");
    }
}
