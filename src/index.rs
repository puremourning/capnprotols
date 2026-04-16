use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use capnp::serialize;

use crate::schema_capnp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
  File,
  Struct,
  Enum,
  Interface,
  Const,
  Annotation,
  Other,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // id/scope_id will be needed for cross-file resolution
pub struct NodeInfo {
  pub id: u64,
  pub display_name: String,
  /// Last component of `display_name` after the file `:` separator. For top-level types
  /// this is just the type name; for the file node itself this is empty.
  pub short_name: String,
  pub kind: NodeKind,
  /// File path the compiler reported the node lives in (extracted from displayName prefix).
  pub file: PathBuf,
  /// Byte range of the node's *declaration* in `file`. Zero when the compiler had no info.
  pub start_byte: u32,
  pub end_byte: u32,
  pub scope_id: u64,
  pub doc_comment: Option<String>,
  /// For generic structs/interfaces (e.g. `struct Foo(T, U)`), the parameter names.
  /// Empty for non-generic types.
  pub parameters: Vec<String>,
  /// For struct nodes, the immediate (non-group) named fields with their rendered types.
  /// Empty for non-structs.
  pub fields: Vec<FieldInfo>,
  /// For annotation nodes, the typeId of the value type (typically a struct whose fields
  /// are the named arguments at the application site).
  pub annotation_value_type: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct FieldInfo {
  pub name: String,
  /// Rendered type string, e.g. `:Text`, `:UInt32`, `:List(Foo)`. Empty for groups
  /// (which we don't render).
  pub type_str: String,
}

#[derive(Debug, Clone, Copy)]
pub struct IdentRef {
  pub start_byte: u32,
  pub end_byte: u32,
  /// Node id this identifier resolves to, or 0 for a member ref we can't follow yet.
  pub target_node_id: u64,
}

#[derive(Debug, Default, Clone)]
pub struct FileIndex {
  /// Source-position resolved identifiers for this file, sorted by `start_byte`.
  pub identifiers: Vec<IdentRef>,
  /// Imports declared in this file: typeId of the imported file -> petname (the path
  /// string as written in the source `import "..."`).
  pub imports: HashMap<u64, String>,
}

#[derive(Debug, Default, Clone)]
pub struct Index {
  pub nodes: HashMap<u64, NodeInfo>,
  /// Keyed by the path the compiler reported (matches `NodeInfo::file`).
  pub files: HashMap<PathBuf, FileIndex>,
}

impl Index {
  pub fn from_cgr_bytes(bytes: &[u8]) -> Result<Self> {
    if bytes.is_empty() {
      return Ok(Self::default());
    }
    let reader = serialize::read_message_from_flat_slice(
      &mut &bytes[..],
      Default::default(),
    )?;
    let cgr =
      reader.get_root::<schema_capnp::code_generator_request::Reader>()?;

    let mut idx = Self::default();

    // Build a side table of source-info byte ranges keyed by node id. For nodes from
    // *imported* files, the Node's own startByte/endByte are zero — only sourceInfo has
    // the position. We prefer the Node's value when present, else fall back to this.
    let mut src_ranges: HashMap<u64, (u32, u32)> = HashMap::new();
    let mut doc_comments: HashMap<u64, String> = HashMap::new();
    for si in cgr.get_source_info()?.iter() {
      let s = si.get_start_byte();
      let e = si.get_end_byte();
      if s != 0 || e != 0 {
        src_ranges.insert(si.get_id(), (s, e));
      }
      if si.has_doc_comment() {
        let dc = si.get_doc_comment()?.to_string()?;
        if !dc.is_empty() {
          doc_comments.insert(si.get_id(), dc);
        }
      }
    }

    // First pass: collect basic metadata for every node so we can render type
    // references that point at other nodes (e.g. a struct field of type `:OtherType`).
    let mut display_name_by_id: HashMap<u64, String> = HashMap::new();
    for node in cgr.get_nodes()?.iter() {
      let dn = node.get_display_name()?.to_string()?;
      let prefix_len = node.get_display_name_prefix_length() as usize;
      let short = dn.get(prefix_len..).unwrap_or("").to_string();
      display_name_by_id
        .insert(node.get_id(), if short.is_empty() { dn } else { short });
    }

    // Nodes
    for node in cgr.get_nodes()?.iter() {
      let id = node.get_id();
      let display_name = node.get_display_name()?.to_string()?;
      let prefix_len = node.get_display_name_prefix_length() as usize;
      let short_name = display_name.get(prefix_len..).unwrap_or("").to_string();
      let file = file_of_display_name(&display_name);
      let mut parameters: Vec<String> = Vec::new();
      if node.has_parameters() {
        for p in node.get_parameters()?.iter() {
          parameters.push(p.get_name()?.to_string()?);
        }
      }
      let mut fields: Vec<FieldInfo> = Vec::new();
      let mut annotation_value_type: Option<u64> = None;
      use schema_capnp::node::Which as NodeWhich;
      let kind = match node.which() {
        Ok(NodeWhich::File(())) => NodeKind::File,
        Ok(NodeWhich::Struct(s)) => {
          for f in s.get_fields()?.iter() {
            let name = f.get_name()?.to_string()?;
            let type_str = match f.which() {
              Ok(schema_capnp::field::Slot(slot)) => {
                let ty = slot.get_type()?;
                format!(":{}", render_type(&ty, &display_name_by_id))
              }
              _ => String::new(),
            };
            fields.push(FieldInfo { name, type_str });
          }
          NodeKind::Struct
        }
        Ok(NodeWhich::Enum(_)) => NodeKind::Enum,
        Ok(NodeWhich::Interface(_)) => NodeKind::Interface,
        Ok(NodeWhich::Const(_)) => NodeKind::Const,
        Ok(NodeWhich::Annotation(a)) => {
          let ty = a.get_type()?;
          annotation_value_type = type_target_id(&ty);
          NodeKind::Annotation
        }
        _ => NodeKind::Other,
      };
      let mut start_byte = node.get_start_byte();
      let mut end_byte = node.get_end_byte();
      if start_byte == 0 && end_byte == 0 {
        if let Some(&(s, e)) = src_ranges.get(&id) {
          start_byte = s;
          end_byte = e;
        }
      }
      idx.nodes.insert(
        id,
        NodeInfo {
          id,
          display_name,
          short_name,
          kind,
          file,
          start_byte,
          end_byte,
          scope_id: node.get_scope_id(),
          doc_comment: doc_comments.remove(&id),
          parameters,
          fields,
          annotation_value_type,
        },
      );
    }

    // Per-file identifier lists
    for req in cgr.get_requested_files()?.iter() {
      let filename = req.get_filename()?.to_string()?;
      let path = PathBuf::from(&filename);
      let mut idents: Vec<IdentRef> = Vec::new();
      if req.has_file_source_info() {
        let fsi = req.get_file_source_info()?;
        for ident in fsi.get_identifiers()?.iter() {
          let target = match ident.which() {
                        Ok(schema_capnp::code_generator_request::requested_file::file_source_info::identifier::TypeId(id)) => id,
                        Ok(schema_capnp::code_generator_request::requested_file::file_source_info::identifier::Member(_)) => 0,
                        _ => 0,
                    };
          idents.push(IdentRef {
            start_byte: ident.get_start_byte(),
            end_byte: ident.get_end_byte(),
            target_node_id: target,
          });
        }
      }
      idents.sort_by_key(|i| i.start_byte);
      let mut imports = HashMap::new();
      for imp in req.get_imports()?.iter() {
        let name = imp.get_name()?.to_string()?;
        imports.insert(imp.get_id(), name);
      }
      idx.files.insert(
        path,
        FileIndex {
          identifiers: idents,
          imports,
        },
      );
    }
    Ok(idx)
  }

  /// All FSI identifiers covering `byte_offset` in `file`, ordered shortest-first.
  pub fn identifiers_at(&self, file: &Path, byte_offset: u32) -> Vec<IdentRef> {
    let Some(fi) = self.lookup_file(file) else {
      return Vec::new();
    };
    let mut hits: Vec<IdentRef> = fi
      .identifiers
      .iter()
      .copied()
      .filter(|i| byte_offset >= i.start_byte && byte_offset < i.end_byte)
      .collect();
    hits.sort_by_key(|i| i.end_byte - i.start_byte);
    hits
  }

  /// Look up the smallest-range identifier at `byte_offset` within `file`.
  pub fn identifier_at(
    &self,
    file: &Path,
    byte_offset: u32,
  ) -> Option<IdentRef> {
    self.identifiers_at(file, byte_offset).into_iter().next()
  }

  /// Find an FSI identifier whose range exactly matches `start..end` within `file`.
  pub fn identifier_in_range(
    &self,
    file: &Path,
    start: u32,
    end: u32,
  ) -> Option<IdentRef> {
    let fi = self.lookup_file(file)?;
    fi.identifiers
      .iter()
      .copied()
      .find(|i| i.start_byte == start && i.end_byte == end)
  }

  pub fn node(&self, id: u64) -> Option<&NodeInfo> {
    self.nodes.get(&id)
  }

  /// Rewrite every reference to `from` (compiler-reported overlay path) into `to` (the
  /// real on-disk path). Compares both with and without the leading `/` since capnp
  /// strips that when reporting absolute paths.
  pub fn remap_file(&mut self, from: &Path, to: &Path) {
    let matches = |p: &Path| paths_match(p, from);
    // Rebuild files map with remapped keys.
    let old = std::mem::take(&mut self.files);
    for (path, fi) in old {
      let new_key = if matches(&path) {
        to.to_path_buf()
      } else {
        path
      };
      self.files.insert(new_key, fi);
    }
    // Rewrite each node's file field.
    for node in self.nodes.values_mut() {
      if matches(&node.file) {
        node.file = to.to_path_buf();
      }
    }
  }

  /// Look up the import petname (path string as written in source) for a typeId, given
  /// the requesting file. Useful when the import's file node isn't in `nodes` because
  /// nothing from it survived to the CGR.
  pub fn import_petname(
    &self,
    requesting: &Path,
    type_id: u64,
  ) -> Option<&str> {
    self
      .lookup_file(requesting)?
      .imports
      .get(&type_id)
      .map(String::as_str)
  }

  /// Resolve `file` against the indexed file map. capnp normalizes absolute paths by
  /// stripping the leading `/`, so an exact match often fails — fall back to comparing
  /// with/without that leading separator and finally by file_name.
  fn lookup_file(&self, file: &Path) -> Option<&FileIndex> {
    if let Some(v) = self.files.get(file) {
      return Some(v);
    }
    let s = file.to_string_lossy();
    let stripped = s.strip_prefix('/').unwrap_or(&s);
    if let Some(v) = self.files.get(Path::new(stripped)) {
      return Some(v);
    }
    let with_slash = format!("/{stripped}");
    if let Some(v) = self.files.get(Path::new(&with_slash)) {
      return Some(v);
    }
    let name = file.file_name()?;
    self
      .files
      .iter()
      .find(|(p, _)| p.file_name() == Some(name))
      .map(|(_, v)| v)
  }

  /// Find a node whose final name component (after the file `:` separator and any
  /// scoping dots) equals `name`. Used as a fallback when the CGR's FSI doesn't have
  /// a position-resolved entry for the cursor (e.g. type parameters inside `List(T)`).
  /// Prefers nodes declared in `prefer_file`, falling back to any match.
  pub fn find_node_by_short_name(
    &self,
    name: &str,
    prefer_file: &Path,
  ) -> Option<&NodeInfo> {
    let mut best: Option<&NodeInfo> = None;
    for n in self.nodes.values() {
      let leaf = n.short_name.rsplit('.').next().unwrap_or(&n.short_name);
      if leaf != name {
        continue;
      }
      if paths_match(&n.file, prefer_file) {
        return Some(n);
      }
      if best.is_none() {
        best = Some(n);
      }
    }
    best
  }

  /// All nodes that look like usable completion candidates: top-level (or nested) named
  /// types/enums/annotations, regardless of file.
  pub fn completion_candidates(&self) -> impl Iterator<Item = &NodeInfo> {
    self.nodes.values().filter(|n| {
      !n.short_name.is_empty()
        && matches!(
          n.kind,
          NodeKind::Struct
            | NodeKind::Enum
            | NodeKind::Interface
            | NodeKind::Annotation
            | NodeKind::Const
        )
    })
  }

  /// Candidates that look like types: structs, enums, interfaces, plus consts (since
  /// you can refer to constants in default values). Excludes annotations.
  pub fn type_candidates(&self) -> impl Iterator<Item = &NodeInfo> {
    self.nodes.values().filter(|n| {
      !n.short_name.is_empty()
        && matches!(
          n.kind,
          NodeKind::Struct
            | NodeKind::Enum
            | NodeKind::Interface
            | NodeKind::Const
        )
    })
  }

  /// Candidates that look like annotations.
  pub fn annotation_candidates(&self) -> impl Iterator<Item = &NodeInfo> {
    self.nodes.values().filter(|n| {
      !n.short_name.is_empty() && matches!(n.kind, NodeKind::Annotation)
    })
  }

  /// Candidates declared directly inside another node (for `Parent.<cursor>` completion
  /// where `Parent` is a local struct/interface/enum). Matches children by `scope_id`
  /// and filters to named declarations worth offering.
  pub fn nested_candidates(&self, parent_id: u64) -> Vec<&NodeInfo> {
    self
      .nodes
      .values()
      .filter(|n| {
        n.scope_id == parent_id
          && !n.short_name.is_empty()
          && !matches!(n.kind, NodeKind::File | NodeKind::Other)
      })
      .collect()
  }

  /// Candidates declared inside a particular file (for `Namespace.<cursor>` completion).
  pub fn candidates_in_file(&self, file: &Path) -> Vec<&NodeInfo> {
    self
      .nodes
      .values()
      .filter(|n| {
        !n.short_name.is_empty()
          && paths_match(&n.file, file)
          && !matches!(n.kind, NodeKind::File | NodeKind::Other)
      })
      .collect()
  }
}

/// Render a `Type` reader (from CGR) as a human-readable string. Falls back to typeId
/// numbers when the referenced node isn't known.
fn render_type(
  ty: &schema_capnp::type_::Reader,
  names: &HashMap<u64, String>,
) -> String {
  use schema_capnp::type_::Which::*;
  match ty.which() {
    Ok(Void(())) => "Void".into(),
    Ok(Bool(())) => "Bool".into(),
    Ok(Int8(())) => "Int8".into(),
    Ok(Int16(())) => "Int16".into(),
    Ok(Int32(())) => "Int32".into(),
    Ok(Int64(())) => "Int64".into(),
    Ok(Uint8(())) => "UInt8".into(),
    Ok(Uint16(())) => "UInt16".into(),
    Ok(Uint32(())) => "UInt32".into(),
    Ok(Uint64(())) => "UInt64".into(),
    Ok(Float32(())) => "Float32".into(),
    Ok(Float64(())) => "Float64".into(),
    Ok(Text(())) => "Text".into(),
    Ok(Data(())) => "Data".into(),
    Ok(List(l)) => match l.get_element_type() {
      Ok(inner) => format!("List({})", render_type(&inner, names)),
      Err(_) => "List(?)".into(),
    },
    Ok(Enum(e)) => name_or_id(names, e.get_type_id()),
    Ok(Struct(s)) => name_or_id(names, s.get_type_id()),
    Ok(Interface(i)) => name_or_id(names, i.get_type_id()),
    Ok(AnyPointer(_)) => "AnyPointer".into(),
    Err(_) => "?".into(),
  }
}

fn name_or_id(names: &HashMap<u64, String>, id: u64) -> String {
  names
    .get(&id)
    .cloned()
    .unwrap_or_else(|| format!("@0x{id:016x}"))
}

/// For type references that point at a single named node (struct/enum/interface), return
/// that node's typeId. Used for annotation value types.
fn type_target_id(ty: &schema_capnp::type_::Reader) -> Option<u64> {
  use schema_capnp::type_::Which::*;
  match ty.which() {
    Ok(Struct(s)) => Some(s.get_type_id()),
    Ok(Enum(e)) => Some(e.get_type_id()),
    Ok(Interface(i)) => Some(i.get_type_id()),
    _ => None,
  }
}

/// True if two paths refer to the same on-disk location. capnp's displayName for nested
/// nodes can be just a basename (no path prefix) while we hold the absolute overlay
/// path, so we accept a basename match as well — the overlay basename is unique enough
/// (`.capnprotols.<file>`) that this doesn't conflate unrelated files.
fn paths_match(a: &Path, b: &Path) -> bool {
  if a == b {
    return true;
  }
  let as_ = a.to_string_lossy();
  let bs = b.to_string_lossy();
  let ans = as_.strip_prefix('/').unwrap_or(&as_);
  let bns = bs.strip_prefix('/').unwrap_or(&bs);
  if ans == bns {
    return true;
  }
  match (a.file_name(), b.file_name()) {
    (Some(x), Some(y)) => x == y,
    _ => false,
  }
}

fn file_of_display_name(display_name: &str) -> PathBuf {
  // displayName format: "path/to/file.capnp" for the file node, or
  // "path/to/file.capnp:Outer.Inner" for nested nodes. The prefix length field tells us
  // exactly where the file part ends (it includes the trailing ':' for nested nodes), but
  // splitting on ':' is robust enough for path extraction.
  let file = match display_name.find(':') {
    Some(i) => &display_name[..i],
    None => display_name,
  };
  PathBuf::from(file)
}

fn _assert_anyhow_used() -> Result<()> {
  Err(anyhow!("unused"))
}
