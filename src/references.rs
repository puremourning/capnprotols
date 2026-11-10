//! Support for `textDocument/references`.
//!
//! Node ids are stable across compilations (they're the explicit or derived `@0x…` ids),
//! and every compile's `fileSourceInfo` lists each identifier in the requested file
//! together with the node id it resolved to. So "find references" is a reverse lookup
//! over those identifier lists: collect them for every schema we know about and keep
//! the ones that point at the target id.
//!
//! Open buffers already have a live index (see `Backend::refresh`). Files that aren't
//! open are covered by [`WorkspaceIndex`], which compiles each schema the workspace
//! scan finds once and caches its identifiers until the file's mtime changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use tokio::sync::{Mutex, Semaphore};
use tracing::{debug, warn};

use crate::compiler;
use crate::config::Config;
use crate::index::{paths_match, IdentRef, Index, NodeInfo};

/// How many `capnp compile` processes a workspace scan runs at once.
const MAX_PARALLEL_COMPILES: usize = 8;

/// Identifiers recorded for one on-disk schema, as of `mtime`.
#[derive(Debug, Clone)]
struct CachedFile {
  mtime:       SystemTime,
  identifiers: Arc<Vec<IdentRef>>,
}

/// Lazily-built, mtime-invalidated identifier lists for every schema in the workspace.
#[derive(Debug, Default)]
pub struct WorkspaceIndex {
  files: Mutex<HashMap<PathBuf, CachedFile>>,
}

impl WorkspaceIndex {
  pub fn new() -> Self {
    Self::default()
  }

  /// Identifier lists for each of `files` (see [`crate::workspace::discover`]), except
  /// those in `skip` (open buffers, whose live index is fresher than the disk copy).
  /// Files that changed since the last call — or were never seen — are recompiled; the
  /// rest come from cache.
  pub async fn identifiers(
    &self,
    config: &Config,
    files: &[PathBuf],
    skip: &[PathBuf],
  ) -> Vec<(PathBuf, Arc<Vec<IdentRef>>)> {
    let mut out = Vec::new();
    let mut stale: Vec<(PathBuf, SystemTime)> = Vec::new();
    {
      let cache = self.files.lock().await;
      for path in files {
        let path = path.clone();
        if skip.iter().any(|s| s == &path) {
          continue;
        }
        let Some(mtime) =
          std::fs::metadata(&path).and_then(|m| m.modified()).ok()
        else {
          continue;
        };
        match cache.get(&path) {
          Some(c) if c.mtime == mtime => {
            out.push((path, c.identifiers.clone()))
          }
          _ => stale.push((path, mtime)),
        }
      }
    }

    if !stale.is_empty() {
      debug!("references: compiling {} workspace schema(s)", stale.len());
      let sem = Arc::new(Semaphore::new(MAX_PARALLEL_COMPILES));
      let mut tasks = tokio::task::JoinSet::new();
      for (path, mtime) in stale {
        let config = config.clone();
        let sem = sem.clone();
        tasks.spawn(async move {
          let _permit = sem.acquire_owned().await.ok()?;
          let idents = compile_identifiers(&config, &path).await?;
          Some((path, mtime, idents))
        });
      }
      let mut cache = self.files.lock().await;
      while let Some(res) = tasks.join_next().await {
        let Ok(Some((path, mtime, idents))) = res else {
          continue;
        };
        let identifiers = Arc::new(idents);
        cache.insert(
          path.clone(),
          CachedFile {
            mtime,
            identifiers: identifiers.clone(),
          },
        );
        out.push((path, identifiers));
      }
    }
    out
  }
}

/// Compile `path` on its own and return the identifiers capnp resolved in it. `None`
/// when the file doesn't produce a usable CGR (e.g. it has errors) — it's simply left
/// out of the results rather than failing the whole request.
async fn compile_identifiers(
  config: &Config,
  path: &Path,
) -> Option<Vec<IdentRef>> {
  let out = match compiler::compile_file(config, path, None, &[]).await {
    Ok(o) => o,
    Err(e) => {
      warn!("references: compiling {} failed: {e:#}", path.display());
      return None;
    }
  };
  if out.cgr.is_empty() {
    debug!("references: no CGR for {}", path.display());
    return None;
  }
  let index = Index::from_cgr_bytes(&out.cgr).ok()?;
  Some(index.identifiers_in(path).to_vec())
}

/// Best-effort canonical form so the same file reached two ways compares equal.
pub fn canonical(path: &Path) -> PathBuf {
  std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The node declared in `file` whose *name* sits under `byte` — i.e. the cursor is on
/// `Foo` in `struct Foo {`. Declarations aren't listed as identifiers in the FSI, so
/// this is how a references request issued from the declaration finds its target.
/// The innermost (shortest-range) match wins.
pub fn declaration_at<'a>(
  index: &'a Index,
  file: &Path,
  text: &str,
  byte: usize,
) -> Option<&'a NodeInfo> {
  index
    .nodes
    .values()
    .filter(|n| n.end_byte > n.start_byte && !n.short_name.is_empty())
    .filter(|n| paths_match(&n.file, file))
    .filter(|n| {
      decl_name_range(n, text).is_some_and(|(s, e)| byte >= s && byte <= e)
    })
    .min_by_key(|n| n.end_byte - n.start_byte)
}

/// Byte range of `node`'s declared name within `text` (the file it's declared in).
pub fn decl_name_range(node: &NodeInfo, text: &str) -> Option<(usize, usize)> {
  locate_decl_name(
    text,
    node.start_byte as usize,
    node.end_byte as usize,
    leaf_name(node),
  )
}

/// The last dotted component of a node's name — `Inner` for `Outer.Inner`.
pub fn leaf_name(node: &NodeInfo) -> &str {
  node
    .short_name
    .rsplit('.')
    .next()
    .unwrap_or(&node.short_name)
}

/// Narrow an identifier span to its final dotted component when it ends with `leaf`,
/// so a reference written `Types.Date` highlights just `Date`. Spans that don't end in
/// the leaf (e.g. a `using` alias spelled differently) are returned unchanged.
pub fn narrow_to_leaf(
  text: &str,
  start: usize,
  end: usize,
  leaf: &str,
) -> (usize, usize) {
  let Some(s) = text.get(start..end) else {
    return (start, end);
  };
  if leaf.is_empty() || s.len() <= leaf.len() || !s.ends_with(leaf) {
    return (start, end);
  }
  let leaf_start = end - leaf.len();
  if text.as_bytes()[leaf_start - 1] == b'.' {
    (leaf_start, end)
  } else {
    (start, end)
  }
}

/// Within `text[start..end]`, find the byte range of the first occurrence of `name` as a
/// whole identifier token. Used to narrow a node's whole-declaration range down to just
/// its name (so goto lands on `Foo` rather than the `struct` keyword).
pub fn locate_decl_name(
  text: &str,
  start: usize,
  end: usize,
  name: &str,
) -> Option<(usize, usize)> {
  let bytes = text.as_bytes();
  let end = end.min(bytes.len());
  if start >= end || name.is_empty() {
    return None;
  }
  let needle = name.as_bytes();
  let is_ident_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
  let mut i = start;
  while i + needle.len() <= end {
    if &bytes[i..i + needle.len()] == needle {
      let before_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
      let after_idx = i + needle.len();
      let after_ok =
        after_idx >= bytes.len() || !is_ident_byte(bytes[after_idx]);
      if before_ok && after_ok {
        return Some((i, after_idx));
      }
    }
    i += 1;
  }
  None
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn narrow_to_leaf_trims_a_qualified_reference() {
    let text = "x @0 :Types.Date;";
    let s = text.find("Types").unwrap();
    let e = s + "Types.Date".len();
    let (ns, ne) = narrow_to_leaf(text, s, e, "Date");
    assert_eq!(&text[ns..ne], "Date");
  }

  #[test]
  fn narrow_to_leaf_keeps_an_unqualified_reference() {
    let text = "x @0 :Date;";
    let s = text.find("Date").unwrap();
    assert_eq!(narrow_to_leaf(text, s, s + 4, "Date"), (s, s + 4));
  }

  #[test]
  fn narrow_to_leaf_ignores_a_suffix_that_is_not_a_component() {
    let text = "x @0 :UpdateDate;";
    let s = text.find("UpdateDate").unwrap();
    let e = s + "UpdateDate".len();
    assert_eq!(narrow_to_leaf(text, s, e, "Date"), (s, e));
  }
}
