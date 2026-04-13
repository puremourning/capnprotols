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
fn strip_comments(src: &str) -> String {
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
