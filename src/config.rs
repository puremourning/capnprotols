use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
  pub enabled:         bool,
  /// Hard column limit. Matches the KJ style guide default.
  pub max_width:       u32,
  /// Publish a `WARNING` Diagnostic when a long line can't be auto-wrapped.
  pub warn_long_lines: bool,
}

impl Default for FormatOptions {
  fn default() -> Self {
    Self {
      enabled:         true,
      max_width:       100,
      warn_long_lines: true,
    }
  }
}

#[derive(Debug, Clone)]
pub struct Config {
  pub compiler_path:    String,
  pub import_paths:     Vec<PathBuf>,
  /// Directories to consult when resolving a compiler-reported file path that doesn't
  /// exist on disk as-is (e.g. `capnp/compat/json.capnp` lives under the capnp install
  /// `include/` directory). Includes user-supplied import_paths plus the capnp install
  /// include root derived from the compiler binary location.
  pub resolution_roots: Vec<PathBuf>,
  pub format:           FormatOptions,
}

impl Config {
  pub fn from_init(opts: Option<InitOptions>) -> Self {
    let opts = opts.unwrap_or_default();
    let compiler_path =
      opts.compiler_path.unwrap_or_else(|| "capnp".to_string());

    // Resolution roots in priority order:
    //   1. user-supplied import paths (LSP initializationOptions.importPaths)
    //   2. the compiler's standard import paths, as reported by `capnp config
    //      --import-paths`. These are exactly the directories capnp searches, in its own
    //      order, so they're kept as-is.
    //   3. if the compiler doesn't support `capnp config` (e.g. a stock release), a best
    //      guess instead: the include dir derived from the resolved capnp binary's install
    //      prefix, capnp's two hardcoded standard paths (/usr/local/include,
    //      /usr/include), then common platform defaults. Each guess is kept only if
    //      `capnp/c++.capnp` exists under it — the canonical "is this a capnp include
    //      root" test, mirroring what capnp itself looks for.
    // Candidates are deduplicated after canonicalization. `trusted` candidates are kept
    // whether or not they contain the standard schema tree (user paths may host unrelated
    // schemas; compiler-reported paths are searched by capnp regardless).
    let mut candidates: Vec<(PathBuf, bool)> = Vec::new();
    candidates.extend(opts.import_paths.iter().map(|p| (p.clone(), true)));
    match query_standard_import_paths(&compiler_path) {
      Some(paths) => {
        candidates.extend(paths.into_iter().map(|p| (p, true)));
      }
      None => {
        if let Some(inc) = derive_capnp_include(&compiler_path) {
          candidates.push((inc, false));
        }
        for guess in [
          "/usr/local/include",
          "/usr/include",
          "/opt/homebrew/include",
          "/opt/local/include", // MacPorts
        ] {
          candidates.push((PathBuf::from(guess), false));
        }
      }
    }

    let mut seen = std::collections::HashSet::new();
    let mut resolution_roots = Vec::new();
    for (c, trusted) in candidates {
      let canon = std::fs::canonicalize(&c).unwrap_or_else(|_| c.clone());
      if !seen.insert(canon.clone()) {
        continue;
      }
      if trusted || canon.join("capnp/c++.capnp").exists() {
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

/// Asks the compiler for the directories it searches for non-relative imports, via
/// `capnp config --import-paths` (one path per line, in search order). Returns `None` if
/// the compiler can't be run or doesn't have the `config` subcommand: it was added
/// alongside relocatable installs and isn't in stock capnp releases, which fail with
/// "config: unknown command".
fn query_standard_import_paths(compiler_path: &str) -> Option<Vec<PathBuf>> {
  let output = Command::new(compiler_path)
    .args(["config", "--import-paths"])
    .stdin(Stdio::null())
    .output()
    .ok()?;
  if !output.status.success() {
    return None;
  }
  Some(parse_import_paths(&String::from_utf8_lossy(&output.stdout)))
}

fn parse_import_paths(stdout: &str) -> Vec<PathBuf> {
  stdout
    .lines()
    .map(str::trim_end) // Windows line endings
    .filter(|line| !line.is_empty())
    .map(PathBuf::from)
    .collect()
}

/// Given a capnp executable name or path, find the corresponding `include/` directory in
/// the same install prefix (e.g. `/opt/homebrew/bin/capnp` -> `/opt/homebrew/include`).
/// Symlinks are resolved first, so `/usr/local/bin/capnp -> /opt/capnp/bin/capnp` maps to
/// `/opt/capnp/include`.
fn derive_capnp_include(compiler_path: &str) -> Option<PathBuf> {
  let found = which(compiler_path)?;
  let resolved = std::fs::canonicalize(&found).unwrap_or(found);
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
  fn parses_import_paths() {
    assert_eq!(
      parse_import_paths("/opt/capnp/include\r\n/usr/include\n\n"),
      vec![
        PathBuf::from("/opt/capnp/include"),
        PathBuf::from("/usr/include")
      ]
    );
    assert!(parse_import_paths("").is_empty());
  }

  /// Writes an executable shell script standing in for `capnp`.
  #[cfg(unix)]
  fn fake_compiler(dir: &Path, script: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("capnp");
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
      .unwrap();
    path.to_string_lossy().into_owned()
  }

  fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
      "capnprotols-config-test-{}-{name}",
      std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
  }

  #[cfg(unix)]
  #[test]
  fn uses_compiler_reported_import_paths() {
    let dir = scratch_dir("reported");
    // Neither directory contains capnp/c++.capnp: compiler-reported paths are kept anyway.
    let a = dir.join("a");
    let b = dir.join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let compiler = fake_compiler(
      &dir,
      &format!(
        "[ \"$1 $2\" = \"config --import-paths\" ] || exit 1\necho {}\necho {}",
        a.display(),
        b.display()
      ),
    );

    let cfg = Config::from_init(Some(InitOptions {
      compiler_path: Some(compiler),
      ..Default::default()
    }));
    assert_eq!(cfg.resolution_roots, vec![a, b]);
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[cfg(unix)]
  #[test]
  fn falls_back_when_compiler_lacks_config() {
    // Lay out an install prefix, reached through a symlink, without `capnp config`.
    let dir = scratch_dir("fallback");
    let prefix = dir.join("prefix");
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    std::fs::create_dir_all(prefix.join("include/capnp")).unwrap();
    std::fs::write(prefix.join("include/capnp/c++.capnp"), "").unwrap();
    let real = fake_compiler(
      &prefix.join("bin"),
      "echo \"capnp: config: unknown command\" >&2; exit 1",
    );
    let link = dir.join("capnp-link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let cfg = Config::from_init(Some(InitOptions {
      compiler_path: Some(link.to_string_lossy().into_owned()),
      ..Default::default()
    }));
    assert_eq!(cfg.resolution_roots.first(), Some(&prefix.join("include")));
    let _ = std::fs::remove_dir_all(&dir);
  }
  #[test]
  fn probe_finds_at_least_one_capnp_include() {
    let cfg = Config::from_init(None);
    eprintln!(
      "compiler={} roots={:?}",
      cfg.compiler_path, cfg.resolution_roots
    );
    assert!(
      !cfg.resolution_roots.is_empty(),
      "expected at least one capnp include root on this system"
    );
  }
}
