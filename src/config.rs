use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct InitOptions {
    /// Path to the `capnp` executable. Defaults to `capnp` on `$PATH`.
    pub compiler_path: Option<String>,

    /// Additional `-I` import paths passed to `capnp compile`.
    pub import_paths: Vec<PathBuf>,

    /// Formatter settings (textDocument/formatting).
    pub format: FormatOptions,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FormatOptions {
    /// Master switch. When false, formatting requests return no edits and we don't
    /// emit long-line warning diagnostics.
    pub enabled: bool,
    /// Hard column limit. Matches the KJ style guide default.
    pub max_width: u32,
    /// Publish a `WARNING` Diagnostic when a long line can't be auto-wrapped.
    pub warn_long_lines: bool,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            max_width: 100,
            warn_long_lines: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub compiler_path: String,
    pub import_paths: Vec<PathBuf>,
    /// Directories to consult when resolving a compiler-reported file path that doesn't
    /// exist on disk as-is (e.g. `capnp/compat/json.capnp` lives under the capnp install
    /// `include/` directory). Includes user-supplied import_paths plus the capnp install
    /// include root derived from the compiler binary location.
    pub resolution_roots: Vec<PathBuf>,
    pub format: FormatOptions,
}

impl Config {
    pub fn from_init(opts: Option<InitOptions>) -> Self {
        let opts = opts.unwrap_or_default();
        let compiler_path = opts.compiler_path.unwrap_or_else(|| "capnp".to_string());

        // Resolution roots in priority order:
        //   1. user-supplied import paths (LSP initializationOptions.importPaths)
        //   2. include dir derived from the resolved capnp binary's install prefix
        //   3. capnp's two hardcoded standard paths (/usr/local/include, /usr/include)
        //   4. common platform defaults probed for actual presence
        // We then probe each candidate by checking whether `capnp/c++.capnp` exists under
        // it — that's the canonical "is this a capnp include root" test, mirroring what
        // capnp itself looks for. Only matching roots are kept (deduplicated).
        let mut candidates: Vec<PathBuf> = Vec::new();
        candidates.extend(opts.import_paths.iter().cloned());
        if let Some(inc) = derive_capnp_include(&compiler_path) {
            candidates.push(inc);
        }
        candidates.push(PathBuf::from("/usr/local/include"));
        candidates.push(PathBuf::from("/usr/include"));
        candidates.push(PathBuf::from("/opt/homebrew/include"));
        candidates.push(PathBuf::from("/opt/local/include")); // MacPorts

        let mut seen = std::collections::HashSet::new();
        let mut resolution_roots = Vec::new();
        for c in candidates {
            let canon = std::fs::canonicalize(&c).unwrap_or_else(|_| c.clone());
            if !seen.insert(canon.clone()) {
                continue;
            }
            // Always keep user-supplied import paths (they may host non-standard schemas
            // unrelated to the bundled capnp/* tree). For all others, keep only roots that
            // actually contain the standard schema tree.
            let is_user = opts.import_paths.iter().any(|p| {
                std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()) == canon
            });
            if is_user || canon.join("capnp/c++.capnp").exists() {
                resolution_roots.push(canon);
            }
        }

        Self {
            compiler_path,
            import_paths: opts.import_paths,
            resolution_roots,
            format: opts.format,
        }
    }
}

/// Given a capnp executable name or path, find the corresponding `include/` directory in
/// the same install prefix (e.g. `/opt/homebrew/bin/capnp` -> `/opt/homebrew/include`).
fn derive_capnp_include(compiler_path: &str) -> Option<PathBuf> {
    let resolved = which(compiler_path)?;
    let bin_dir = resolved.parent()?;
    let prefix = bin_dir.parent()?;
    let inc = prefix.join("include");
    inc.is_dir().then_some(inc)
}

fn which(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.is_absolute() {
        return p.exists().then(|| p.to_path_buf());
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn probe_finds_at_least_one_capnp_include() {
        let cfg = Config::from_init(None);
        eprintln!("compiler={} roots={:?}", cfg.compiler_path, cfg.resolution_roots);
        assert!(!cfg.resolution_roots.is_empty(), "expected at least one capnp include root on this system");
    }
}
