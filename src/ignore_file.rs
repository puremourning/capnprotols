//! `.capnpfmtignore` — gitignore-style file exclusion for `capnpfmt`.
//!
//! For each input file, walk up its ancestor directories collecting any
//! `.capnpfmtignore` files. Stop at a `.git` directory (mirrors gitignore's
//! repo-root boundary) or the filesystem root. Apply the collected matchers
//! in root-to-leaf order so leaf rules can whitelist with `!`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::Match;

const IGNORE_FILENAME: &str = ".capnpfmtignore";

pub struct Ignore {
  matchers: Vec<Gitignore>,
}

impl Ignore {
  /// Discover `.capnpfmtignore` files walking up from `start_dir`.
  pub fn discover(start_dir: &Path) -> Self {
    let mut matchers = Vec::new();
    let mut dir: Option<&Path> = Some(start_dir);
    while let Some(d) = dir {
      let candidate = d.join(IGNORE_FILENAME);
      if candidate.is_file() {
        let mut b = GitignoreBuilder::new(d);
        let _ = b.add(&candidate);
        if let Ok(gi) = b.build() {
          matchers.push(gi);
        }
      }
      if d.join(".git").exists() {
        break;
      }
      dir = d.parent();
    }
    matchers.reverse();
    Self { matchers }
  }

  /// True if `path` is matched by any discovered ignore rule (accounting for
  /// `!` whitelist overrides in deeper files). Uses
  /// `matched_path_or_any_parents` so a directory pattern like `vendor/`
  /// excludes everything beneath it, matching gitignore intent.
  pub fn is_ignored(&self, path: &Path) -> bool {
    let mut decision = Match::None;
    for gi in &self.matchers {
      let m = gi.matched_path_or_any_parents(path, false);
      if !matches!(m, Match::None) {
        decision = m;
      }
    }
    matches!(decision, Match::Ignore(_))
  }
}

/// Caches discovery by parent directory so a batch of files in the same dir
/// only walks the tree once.
#[derive(Default)]
pub struct IgnoreCache {
  by_dir: HashMap<PathBuf, Arc<Ignore>>,
}

impl IgnoreCache {
  pub fn new() -> Self {
    Self::default()
  }

  /// True if `path` is ignored. Canonicalizes `path` so absolute matching
  /// against the discovered ignore-file roots is unambiguous.
  pub fn is_ignored(&mut self, path: &Path) -> bool {
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let parent = abs.parent().unwrap_or_else(|| Path::new("/"));
    let ig = if let Some(ig) = self.by_dir.get(parent) {
      ig.clone()
    } else {
      let ig = Arc::new(Ignore::discover(parent));
      self.by_dir.insert(parent.to_path_buf(), ig.clone());
      ig
    };
    ig.is_ignored(&abs)
  }
}

#[cfg(test)]
mod tests {
  use std::fs;

  use super::*;

  fn tempdir(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map(|d| d.as_nanos())
      .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
      "capnpfmt-ignore-{}-{}-{nanos}",
      label,
      std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    // Pretend this is the repo root so discovery stops here.
    fs::create_dir_all(dir.join(".git")).unwrap();
    dir
  }

  #[test]
  fn matches_simple_pattern_in_subdir() {
    let root = tempdir("simple");
    fs::write(root.join(".capnpfmtignore"), "vendor/\n").unwrap();
    fs::create_dir_all(root.join("vendor")).unwrap();
    fs::write(root.join("vendor/foo.capnp"), "").unwrap();
    fs::write(root.join("bar.capnp"), "").unwrap();

    let mut cache = IgnoreCache::new();
    assert!(cache.is_ignored(&root.join("vendor/foo.capnp")));
    assert!(!cache.is_ignored(&root.join("bar.capnp")));

    fs::remove_dir_all(&root).unwrap();
  }

  #[test]
  fn whitelist_in_nested_ignore_overrides_parent() {
    let root = tempdir("nested");
    fs::write(root.join(".capnpfmtignore"), "*.capnp\n").unwrap();
    fs::create_dir_all(root.join("keep")).unwrap();
    fs::write(root.join("keep/.capnpfmtignore"), "!*.capnp\n").unwrap();
    fs::write(root.join("skip.capnp"), "").unwrap();
    fs::write(root.join("keep/me.capnp"), "").unwrap();

    let mut cache = IgnoreCache::new();
    assert!(cache.is_ignored(&root.join("skip.capnp")));
    assert!(!cache.is_ignored(&root.join("keep/me.capnp")));

    fs::remove_dir_all(&root).unwrap();
  }

  #[test]
  fn discovery_stops_at_git_root() {
    let root = tempdir("boundary");
    // An ignore file outside the .git boundary must not apply.
    let outside = root.parent().unwrap().join(".capnpfmtignore");
    let _ = fs::write(&outside, "*.capnp\n");
    fs::write(root.join("inside.capnp"), "").unwrap();

    let mut cache = IgnoreCache::new();
    assert!(!cache.is_ignored(&root.join("inside.capnp")));

    let _ = fs::remove_file(&outside);
    fs::remove_dir_all(&root).unwrap();
  }
}
