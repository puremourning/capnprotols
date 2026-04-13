use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};

fn line_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Capnp emits any of:
    //   path:line:col: error: msg
    //   path:line:col-col: error: msg              (column range, same line)
    //   path:line:col-line:col: error: msg         (range spanning lines)
    //   path:line: error: msg                      (line only, no column)
    // The end-anchor on the optional ranges keeps them from greedily eating into the
    // severity. `c2only` and (`l2` + `c2`) are mutually exclusive.
    RE.get_or_init(|| {
        Regex::new(
            r"^(?P<path>[^:]+):(?P<l1>\d+)(?::(?P<c1>\d+)(?:-(?:(?P<l2>\d+):(?P<c2>\d+)|(?P<c2only>\d+)))?)?:\s*(?P<sev>error|warning|fatal|note):\s*(?P<msg>.*)$",
        )
        .unwrap()
    })
}

/// Parse `capnp compile` stderr lines of the form:
///   path/to/file.capnp:LINE[:COL][-LINE2:COL2]: severity: message
/// Returns diagnostics scoped to `file_path` only.
pub fn parse_stderr(stderr: &str, file_path: &Path) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let target = file_path.to_string_lossy();
    for line in stderr.lines() {
        let Some(caps) = line_re().captures(line) else { continue };
        let path = &caps["path"];
        if !same_path(path, &target) {
            continue;
        }
        let l1 = caps["l1"].parse::<u32>().unwrap_or(1).saturating_sub(1);
        let c1 = caps
            .name("c1")
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .unwrap_or(1)
            .saturating_sub(1);
        let l2 = caps
            .name("l2")
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .map(|v| v.saturating_sub(1))
            .unwrap_or(l1);
        // `c2` is the explicit `line:col` end column; `c2only` is the `-COL` shorthand
        // when the range lives on the same line.
        let c2 = caps
            .name("c2")
            .or_else(|| caps.name("c2only"))
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .map(|v| v.saturating_sub(1))
            .unwrap_or(c1 + 1);
        let severity = match &caps["sev"] {
            "error" | "fatal" => DiagnosticSeverity::ERROR,
            "warning" => DiagnosticSeverity::WARNING,
            "note" => DiagnosticSeverity::INFORMATION,
            _ => DiagnosticSeverity::ERROR,
        };
        out.push(Diagnostic {
            range: Range {
                start: Position::new(l1, c1),
                end: Position::new(l2, c2),
            },
            severity: Some(severity),
            source: Some("capnp".to_string()),
            message: caps["msg"].trim().to_string(),
            ..Default::default()
        });
    }
    out
}

fn same_path(a: &str, b: &str) -> bool {
    let pa = Path::new(a);
    let pb = Path::new(b);
    if pa == pb {
        return true;
    }
    if let (Ok(ca), Ok(cb)) = (pa.canonicalize(), pb.canonicalize()) {
        if ca == cb {
            return true;
        }
    }
    pa.file_name() == pb.file_name()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parses_basic_error() {
        let stderr = "foo.capnp:3:5: error: bad thing\n";
        let diags = parse_stderr(stderr, &PathBuf::from("foo.capnp"));
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "bad thing");
        assert_eq!(diags[0].range.start.line, 2);
        assert_eq!(diags[0].range.start.character, 4);
    }

    #[test]
    fn parses_range() {
        let stderr = "foo.capnp:3:5-3:9: error: bad\n";
        let diags = parse_stderr(stderr, &PathBuf::from("foo.capnp"));
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].range.end.character, 8);
    }

    #[test]
    fn parses_column_only_range() {
        // capnp's typical output for a same-line range: `path:LINE:COL-COL: error: ...`
        let stderr = "foo.capnp:2:22-32: error: Not defined: NoSuchType\n";
        let diags = parse_stderr(stderr, &PathBuf::from("foo.capnp"));
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].range.start.line, 1);
        assert_eq!(diags[0].range.start.character, 21);
        assert_eq!(diags[0].range.end.line, 1);
        assert_eq!(diags[0].range.end.character, 31);
        assert_eq!(diags[0].message, "Not defined: NoSuchType");
    }

    #[test]
    fn ignores_other_files() {
        let stderr = "other.capnp:1:1: error: nope\n";
        let diags = parse_stderr(stderr, &PathBuf::from("foo.capnp"));
        assert!(diags.is_empty());
    }
}
