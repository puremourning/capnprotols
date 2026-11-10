//! Discovering the schemas that make up a workspace.
//!
//! Cap'n Proto has no project file, so the set of schemas is whatever lives under the
//! workspace folders, narrowed by the user's `workspace.include` / `workspace.exclude`
//! globs (see [`WorkspaceOptions`]). Patterns are relative to each workspace folder
//! unless absolute. A pattern that matches a directory takes in everything under it, so
//! `foo/*/schema` and `foo/*/schema/**` mean the same thing.
//!
//! On a large repo the walk itself is the pathological part, so it prunes as it goes:
//! each include pattern walks only from its literal prefix (`foo/` for
//! `foo/*/schema/**`), and a directory that can't lead to a match (`foo/x/docs/`) or
//! that matches an exclude is never entered. A hard `maxFiles` cap bounds whatever is
//! left.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use globset::{GlobBuilder, GlobMatcher, GlobSet, GlobSetBuilder};
use tracing::warn;

use crate::config::WorkspaceOptions;

/// Excludes applied on top of the user's. User excludes are added to these rather than
/// replacing them, so configuring one exclude doesn't silently re-admit `node_modules`.
pub const DEFAULT_EXCLUDES: &[&str] = &["**/node_modules", "**/.*"];

#[derive(Debug, Default)]
pub struct Discovery {
  pub files:     Vec<PathBuf>,
  /// True when the walk stopped at `maxFiles` with schemas still unvisited.
  pub truncated: bool,
}

/// One include pattern, split at its literal prefix.
struct Pattern {
  /// Directory the walk starts from: the root joined with the pattern's leading
  /// glob-free components.
  base:       PathBuf,
  /// The whole pattern, anchored at `base`.
  full:       GlobMatcher,
  /// The pattern's components after `base`, for deciding whether a directory can still
  /// lead to a match.
  components: Vec<Segment>,
}

enum Segment {
  /// `**` — anything at any depth from here on can match.
  AnyDepth,
  Glob(GlobMatcher),
}

impl Pattern {
  /// Whether some path inside `dir` could match. `dir` is either an ancestor of `base`
  /// (we're still walking down to it) or somewhere beneath it.
  fn could_match_under(&self, dir: &Path) -> bool {
    if self.base.starts_with(dir) {
      return true;
    }
    let Ok(rel) = dir.strip_prefix(&self.base) else {
      return false;
    };
    for (i, comp) in rel.components().enumerate() {
      let name = comp.as_os_str().to_string_lossy();
      match self.components.get(i) {
        // Every pattern component matched an ancestor: we're inside a matched
        // directory, which includes everything beneath it.
        None | Some(Segment::AnyDepth) => return true,
        Some(Segment::Glob(g)) => {
          if !g.is_match(&*name) {
            return false;
          }
        }
      }
    }
    true
  }

  /// Whether `file`, or a directory containing it, matches the whole pattern.
  fn includes(&self, file: &Path) -> bool {
    file
      .ancestors()
      .take_while(|p| p.starts_with(&self.base))
      .any(|p| self.full.is_match(p))
  }
}

/// Every `.capnp` file under `roots` that the include/exclude patterns admit, in walk
/// order, up to `opts.max_files`. `max_depth` limits how far below each root the walk
/// goes (used for the "no workspace folders" fallback, which scans only siblings).
pub fn discover(
  roots: &[PathBuf],
  opts: &WorkspaceOptions,
  max_depth: Option<usize>,
) -> Discovery {
  let mut patterns: Vec<Pattern> = Vec::new();
  let mut excludes = GlobSetBuilder::new();
  let default_include = ["**".to_string()];
  let includes: &[String] = if opts.include.is_empty() {
    &default_include
  } else {
    &opts.include
  };
  for root in roots {
    for pat in includes {
      match compile_pattern(root, pat) {
        Some(p) => patterns.push(p),
        None => warn!("workspace.include: invalid pattern {pat:?}"),
      }
    }
    let user = opts.exclude.iter().map(String::as_str);
    for pat in DEFAULT_EXCLUDES.iter().copied().chain(user) {
      match compile_pattern(root, pat) {
        Some(p) => {
          excludes.add(p.full.glob().clone());
        }
        None => warn!("workspace.exclude: invalid pattern {pat:?}"),
      }
    }
  }
  let excludes = excludes.build().unwrap_or_else(|_| GlobSet::empty());
  let patterns = Arc::new(patterns);
  let excludes = Arc::new(excludes);

  // Walk each distinct base once; a base nested inside another is covered by the outer
  // walk.
  let mut bases: Vec<PathBuf> =
    patterns.iter().map(|p| p.base.clone()).collect();
  bases.sort();
  bases.dedup();
  let bases: Vec<PathBuf> = bases
    .iter()
    .filter(|b| !bases.iter().any(|o| o != *b && b.starts_with(o)))
    .filter(|b| b.is_dir())
    .cloned()
    .collect();

  let mut out = Discovery::default();
  let mut seen: HashSet<PathBuf> = HashSet::new();
  'walk: for base in &bases {
    let filter_patterns = patterns.clone();
    let filter_excludes = excludes.clone();
    let walker = ignore::WalkBuilder::new(base)
      // Dot-directories are handled by the `**/.*` default exclude, so the result
      // depends only on the configured patterns (plus .gitignore if enabled).
      .hidden(false)
      .git_ignore(opts.use_git_ignore)
      .git_global(opts.use_git_ignore)
      .git_exclude(opts.use_git_ignore)
      .ignore(opts.use_git_ignore)
      .parents(opts.use_git_ignore)
      .max_depth(max_depth)
      .filter_entry(move |entry| {
        if entry.depth() == 0 || !entry.file_type().is_some_and(|t| t.is_dir())
        {
          return true;
        }
        let dir = entry.path();
        !filter_excludes.is_match(dir)
          && filter_patterns.iter().any(|p| p.could_match_under(dir))
      })
      .build();
    for entry in walker.flatten() {
      if !entry.file_type().is_some_and(|t| t.is_file()) {
        continue;
      }
      let path = entry.path();
      if path.extension().and_then(|e| e.to_str()) != Some("capnp")
        || excludes.is_match(path)
        || !patterns.iter().any(|p| p.includes(path))
      {
        continue;
      }
      let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.into());
      if !seen.insert(canon.clone()) {
        continue;
      }
      if out.files.len() >= opts.max_files {
        out.truncated = true;
        break 'walk;
      }
      out.files.push(canon);
    }
  }
  out
}

/// Anchor `pattern` at `root` (unless it's absolute) and split off its literal prefix.
fn compile_pattern(root: &Path, pattern: &str) -> Option<Pattern> {
  let pattern = pattern.trim_start_matches("./").trim_end_matches('/');
  let (mut base, rest) = if Path::new(pattern).is_absolute() {
    (PathBuf::from("/"), pattern.trim_start_matches('/'))
  } else {
    (root.to_path_buf(), pattern)
  };
  let mut components: Vec<&str> = rest
    .split('/')
    .filter(|c| !c.is_empty() && *c != ".")
    .collect();
  // Move leading literal components into the base so the walk starts as deep as it can.
  while let Some(first) = components.first() {
    if is_glob(first) || *first == ".." {
      break;
    }
    base.push(first);
    components.remove(0);
  }
  // `..` in a pattern would make the base walk outside the root; resolve it lexically.
  if components.contains(&"..") {
    return None;
  }
  let base = normalize(&base);
  let base_glob = globset::escape(&base.to_string_lossy());
  let full = if components.is_empty() {
    base_glob
  } else {
    format!(
      "{}/{}",
      base_glob.trim_end_matches('/'),
      components.join("/")
    )
  };
  let full = glob(&full)?;
  let components = components
    .iter()
    .map(|c| {
      if *c == "**" {
        Some(Segment::AnyDepth)
      } else {
        glob(c).map(Segment::Glob)
      }
    })
    .collect::<Option<Vec<_>>>()?;
  Some(Pattern {
    base,
    full,
    components,
  })
}

fn glob(s: &str) -> Option<GlobMatcher> {
  GlobBuilder::new(s)
    .literal_separator(true)
    .build()
    .ok()
    .map(|g| g.compile_matcher())
}

fn is_glob(component: &str) -> bool {
  component.contains(['*', '?', '[', '{', '\\'])
}

/// Lexically resolve `.` and `..` so `starts_with` comparisons against walked paths work.
fn normalize(path: &Path) -> PathBuf {
  let mut out = PathBuf::new();
  for c in path.components() {
    match c {
      Component::CurDir => {}
      Component::ParentDir => {
        out.pop();
      }
      other => out.push(other),
    }
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  struct Tree(PathBuf);
  impl Tree {
    fn new(name: &str, files: &[&str]) -> Self {
      let dir = std::env::temp_dir().join(format!(
        "capnprotols-workspace-test-{}-{name}",
        std::process::id()
      ));
      let _ = std::fs::remove_dir_all(&dir);
      for f in files {
        let p = dir.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "").unwrap();
      }
      Self(std::fs::canonicalize(&dir).unwrap())
    }
    fn discover(&self, opts: &WorkspaceOptions) -> Vec<String> {
      let mut files: Vec<String> =
        discover(std::slice::from_ref(&self.0), opts, None)
          .files
          .iter()
          .map(|f| f.strip_prefix(&self.0).unwrap().display().to_string())
          .collect();
      files.sort();
      files
    }
  }
  impl Drop for Tree {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }

  fn opts(include: &[&str], exclude: &[&str]) -> WorkspaceOptions {
    WorkspaceOptions {
      include: include.iter().map(|s| s.to_string()).collect(),
      exclude: exclude.iter().map(|s| s.to_string()).collect(),
      ..Default::default()
    }
  }

  const FILES: &[&str] = &[
    "top.capnp",
    "foo/a/schema/one.capnp",
    "foo/a/schema/deep/two.capnp",
    "foo/b/schema/three.capnp",
    "foo/b/docs/nope.capnp",
    "foo/b/schema/readme.md",
    "bar/four.capnp",
    "node_modules/pkg/x.capnp",
    ".hidden/y.capnp",
    "foo/a/schema/.capnprotols.one.capnp",
  ];

  #[test]
  fn default_scans_everything_but_default_excludes() {
    let t = Tree::new("default", FILES);
    assert_eq!(
      t.discover(&WorkspaceOptions::default()),
      vec![
        "bar/four.capnp",
        "foo/a/schema/deep/two.capnp",
        "foo/a/schema/one.capnp",
        "foo/b/docs/nope.capnp",
        "foo/b/schema/three.capnp",
        "top.capnp",
      ]
    );
  }

  #[test]
  fn include_glob_and_directory_forms_agree() {
    let t = Tree::new("include", FILES);
    let expected = vec![
      "foo/a/schema/deep/two.capnp",
      "foo/a/schema/one.capnp",
      "foo/b/schema/three.capnp",
    ];
    assert_eq!(t.discover(&opts(&["foo/*/schema/**"], &[])), expected);
    assert_eq!(t.discover(&opts(&["foo/*/schema"], &[])), expected);
    assert_eq!(t.discover(&opts(&["./foo/*/schema/"], &[])), expected);
  }

  #[test]
  fn single_star_does_not_cross_directories() {
    let t = Tree::new("star", FILES);
    assert_eq!(
      t.discover(&opts(&["foo/*/schema/*.capnp"], &[])),
      vec!["foo/a/schema/one.capnp", "foo/b/schema/three.capnp"]
    );
  }

  #[test]
  fn user_excludes_add_to_defaults() {
    let t = Tree::new("exclude", FILES);
    assert_eq!(
      t.discover(&opts(&[], &["foo/**/deep", "bar"])),
      vec![
        "foo/a/schema/one.capnp",
        "foo/b/docs/nope.capnp",
        "foo/b/schema/three.capnp",
        "top.capnp",
      ]
    );
  }

  #[test]
  fn absolute_patterns_are_used_as_is() {
    let t = Tree::new("absolute", FILES);
    let abs = format!("{}/bar", t.0.display());
    assert_eq!(t.discover(&opts(&[&abs], &[])), vec!["bar/four.capnp"]);
  }

  #[test]
  fn max_files_truncates() {
    let t = Tree::new("cap", FILES);
    let o = WorkspaceOptions {
      max_files: 2,
      ..Default::default()
    };
    let d = discover(std::slice::from_ref(&t.0), &o, None);
    assert_eq!(d.files.len(), 2);
    assert!(d.truncated);
  }

  #[test]
  fn pruning_rejects_non_matching_directories() {
    let root = Path::new("/r");
    let p = compile_pattern(root, "foo/*/schema/**").unwrap();
    assert_eq!(p.base, Path::new("/r/foo"));
    assert!(p.could_match_under(Path::new("/r")));
    assert!(p.could_match_under(Path::new("/r/foo/a")));
    assert!(p.could_match_under(Path::new("/r/foo/a/schema/x/y")));
    assert!(!p.could_match_under(Path::new("/r/foo/a/docs")));
    assert!(!p.could_match_under(Path::new("/r/bar")));
  }
}
