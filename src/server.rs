use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use ropey::Rope;
use tokio::sync::RwLock;
use tower_lsp::jsonrpc::Result as RpcResult;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use tracing::{debug, info, warn};

use crate::aliases;
use crate::compiler;
use crate::config::{Config, InitOptions};
use crate::diagnostics;
use crate::document::DocumentStore;
use crate::index::{Index, NodeInfo, NodeKind};
use crate::ordinals;
use crate::semantic_tokens;

pub struct Backend {
  client: Client,
  docs: DocumentStore,
  config: Arc<RwLock<Config>>,
  /// Per-file symbol index, keyed by the file's URI.
  indices: Arc<DashMap<Url, Arc<Index>>>,
}

impl Backend {
  pub fn new(client: Client) -> Self {
    Self {
      client,
      docs: DocumentStore::new(),
      config: Arc::new(RwLock::new(Config::from_init(None))),
      indices: Arc::new(DashMap::new()),
    }
  }

  /// Resolve `Receiver.Member` where `Member` is a `using` alias declared in the file
  /// that `Receiver` was imported from. Returns the location of the alias declaration if
  /// such a redirect applies, else None.
  #[allow(clippy::too_many_arguments)]
  fn try_imported_alias(
    &self,
    text: &str,
    path: &Path,
    word_start: usize,
    word_end: usize,
    word: &str,
    index: &Index,
    config: &Config,
  ) -> Option<Location> {
    // The cursor word must be preceded by `.` (so it's a member access).
    if word_start == 0 || text.as_bytes()[word_start - 1] != b'.' {
      return None;
    }
    // Find the longest FSI ident that ends at `word_end` and contains `word_start - 1`
    // — that's the dotted form (`Types.UUID`).
    let dot_byte = (word_start - 1) as u32;
    let dotted = index
      .identifiers_at(path, dot_byte)
      .into_iter()
      .find(|i| i.end_byte as usize == word_end)?;
    // The receiver is everything in `dotted` up to `word_start - 1`.
    let receiver_start = dotted.start_byte;
    let receiver_end = (word_start - 1) as u32;
    if receiver_end <= receiver_start {
      return None;
    }
    let recv_ident =
      index.identifier_in_range(path, receiver_start, receiver_end)?;
    // The receiver's file may or may not have a Node entry — if nothing from it ended
    // up in the CGR, only the import petname is available.
    let receiver_file: PathBuf = match index.node(recv_ident.target_node_id) {
      Some(n) if n.kind == NodeKind::File => n.file.clone(),
      _ => match index.import_petname(path, recv_ident.target_node_id) {
        Some(name) => PathBuf::from(name.trim_start_matches('/')),
        None => return None,
      },
    };
    let target_path =
      resolve_target_file(&receiver_file, path, &config.resolution_roots);
    let target_text = std::fs::read_to_string(&target_path).ok()?;
    let aliases_in_target = aliases::scan(&target_text);
    let alias = aliases::find(&aliases_in_target, word)?;
    let target_uri = Url::from_file_path(&target_path).ok()?;
    let target_rope = Rope::from_str(&target_text);
    let start = byte_to_position(&target_rope, alias.name_start_byte);
    let end = byte_to_position(&target_rope, alias.name_end_byte);
    Some(Location {
      uri: target_uri,
      range: Range { start, end },
    })
  }

  async fn refresh(&self, uri: Url) {
    let Some(text) = self.docs.get_text(&uri) else {
      return;
    };
    let Ok(path) = uri.to_file_path() else {
      warn!("non-file URI, skipping: {uri}");
      return;
    };
    let config = self.config.read().await.clone();
    debug!("refresh: compiling {}", path.display());
    let result = compiler::compile_file(&config, &path, Some(&text)).await;
    // Strategy: always update diagnostics, but only replace the cached symbol index
    // when we got a usable CGR. On compile failure we keep the previous index so
    // completion/goto/hover stay useful while the user has a syntax error mid-edit
    // (byte offsets may be stale but "right enough" for everything except very
    // large edits).
    let diags;
    let mut new_index: Option<Arc<Index>> = None;
    match result {
      Ok(out) => {
        // capnp prints paths relative to its cwd, so an absolute overlay path may
        // come back as `/abs/path/.capnprotols.foo.capnp`, `path/.capnprotols.foo.capnp`,
        // or just `.capnprotols.foo.capnp`. Rewriting the overlay *basename* back to
        // the real basename normalises all three at once. The diagnostics matcher
        // then compares by file_name as a fallback so the path prefix doesn't matter.
        let mut stderr = out.stderr.clone();
        if let (Some(ov), Some(real)) = (
          out.overlay_path.as_deref().and_then(|p| p.file_name()),
          path.file_name(),
        ) {
          stderr =
            stderr.replace(&*ov.to_string_lossy(), &real.to_string_lossy());
        }
        diags = diagnostics::parse_stderr(&stderr, &path);
        debug!(
          "refresh: stderr={} bytes, parsed {} diagnostic(s), success={}",
          stderr.len(),
          diags.len(),
          out.success,
        );
        if !stderr.is_empty() && diags.is_empty() {
          let preview: String =
            stderr.lines().take(3).collect::<Vec<_>>().join(" | ");
          warn!("refresh: stderr was non-empty but no diagnostics parsed; first lines: {preview}");
        }
        if !out.cgr.is_empty() {
          match Index::from_cgr_bytes(&out.cgr) {
            Ok(mut i) => {
              if let Some(ov) = out.overlay_path.as_deref() {
                i.remap_file(ov, &path);
              }
              new_index = Some(Arc::new(i));
            }
            Err(e) => warn!("CGR decode failed (keeping cached index): {e:#}"),
          }
        }
      }
      Err(err) => {
        warn!("compile failed: {err:#}");
        diags = vec![Diagnostic {
          range: Range::default(),
          severity: Some(DiagnosticSeverity::ERROR),
          source: Some("capnprotols".to_string()),
          message: format!("failed to invoke capnp: {err}"),
          ..Default::default()
        }];
      }
    }
    if let Some(idx) = new_index {
      self.indices.insert(uri.clone(), idx);
    }
    // No previous index either? Drop in an empty one so downstream reads don't fail.
    self
      .indices
      .entry(uri.clone())
      .or_insert_with(|| Arc::new(Index::default()));
    self.client.publish_diagnostics(uri, diags, None).await;
  }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
  async fn initialize(
    &self,
    params: InitializeParams,
  ) -> RpcResult<InitializeResult> {
    if let Some(value) = params.initialization_options {
      match serde_json::from_value::<InitOptions>(value) {
        Ok(opts) => {
          *self.config.write().await = Config::from_init(Some(opts));
        }
        Err(e) => warn!("invalid initializationOptions: {e}"),
      }
    }
    let cfg = self.config.read().await;
    info!(
            "capnprotols {} initialized: compiler={}, import_paths={:?}, resolution_roots={:?}",
            env!("CARGO_PKG_VERSION"),
            cfg.compiler_path,
            cfg.import_paths,
            cfg.resolution_roots,
        );
    Ok(InitializeResult {
      server_info: Some(ServerInfo {
        name: "capnprotols".to_string(),
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
      }),
      capabilities: ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(
          TextDocumentSyncKind::INCREMENTAL,
        )),
        definition_provider: Some(OneOf::Left(true)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        document_formatting_provider: Some(OneOf::Left(true)),
        signature_help_provider: Some(SignatureHelpOptions {
          trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
          retrigger_characters: Some(vec![",".to_string()]),
          work_done_progress_options: Default::default(),
        }),
        semantic_tokens_provider: Some(
          SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
              legend: SemanticTokensLegend {
                token_types: semantic_tokens::TOKEN_TYPES.to_vec(),
                token_modifiers: semantic_tokens::TOKEN_MODIFIERS.to_vec(),
              },
              full: Some(SemanticTokensFullOptions::Bool(true)),
              range: None,
              ..Default::default()
            },
          ),
        ),
        completion_provider: Some(CompletionOptions {
          trigger_characters: Some(vec![
            ":".to_string(),
            ".".to_string(),
            "$".to_string(),
            "@".to_string(),
          ]),
          ..Default::default()
        }),
        ..Default::default()
      },
    })
  }

  async fn initialized(&self, _: InitializedParams) {
    self
      .client
      .log_message(MessageType::INFO, "capnprotols ready")
      .await;
  }

  async fn shutdown(&self) -> RpcResult<()> {
    Ok(())
  }

  async fn did_open(&self, params: DidOpenTextDocumentParams) {
    let uri = params.text_document.uri.clone();
    self.docs.open(
      uri.clone(),
      params.text_document.text,
      params.text_document.version,
    );
    self.refresh(uri).await;
  }

  async fn did_change(&self, params: DidChangeTextDocumentParams) {
    let uri = params.text_document.uri.clone();
    self.docs.update(
      &uri,
      params.text_document.version,
      params.content_changes,
    );
    self.refresh(uri).await;
  }

  async fn did_save(&self, params: DidSaveTextDocumentParams) {
    // Recompile against the freshly-saved on-disk content.
    self.refresh(params.text_document.uri).await;
  }

  async fn did_close(&self, params: DidCloseTextDocumentParams) {
    self.docs.close(&params.text_document.uri);
    self.indices.remove(&params.text_document.uri);
    self
      .client
      .publish_diagnostics(params.text_document.uri, vec![], None)
      .await;
  }

  async fn goto_definition(
    &self,
    params: GotoDefinitionParams,
  ) -> RpcResult<Option<GotoDefinitionResponse>> {
    let uri = params
      .text_document_position_params
      .text_document
      .uri
      .clone();
    let pos = params.text_document_position_params.position;
    let Some(text) = self.docs.get_text(&uri) else {
      return Ok(None);
    };
    let Ok(path) = uri.to_file_path() else {
      return Ok(None);
    };
    let Some(index) = self.indices.get(&uri).map(|e| e.clone()) else {
      return Ok(None);
    };

    let rope = Rope::from_str(&text);
    let byte = position_to_byte(&rope, pos);
    let config = self.config.read().await.clone();

    // 0. Cursor inside an `import "..."` string literal — jump to the resolved file.
    if let Some(import_path) = import_string_at_byte(&text, byte as usize) {
      let reported = PathBuf::from(import_path.trim_start_matches('/'));
      let target_path =
        resolve_target_file(&reported, &path, &config.resolution_roots);
      if target_path.exists() {
        if let Ok(target_uri) = Url::from_file_path(&target_path) {
          return Ok(Some(GotoDefinitionResponse::Scalar(Location {
            uri: target_uri,
            range: Range::default(),
          })));
        }
      }
    }

    // The cursor's identifier text + its byte span in the source.
    let cursor_span = identifier_span_at_byte(&text, byte as usize);

    // 1. Local `using` alias in the same file — capnp inlines these in the CGR.
    if let Some((_, _, word)) = cursor_span {
      let local_aliases = aliases::scan(&text);
      if let Some(alias) = aliases::find(&local_aliases, word) {
        let start = byte_to_position(&rope, alias.name_start_byte);
        let end = byte_to_position(&rope, alias.name_end_byte);
        return Ok(Some(GotoDefinitionResponse::Scalar(Location {
          uri,
          range: Range { start, end },
        })));
      }
    }

    // 2. Dotted reference `Receiver.Member` where Member happens to be a `using`
    //    alias defined in Receiver's file. The CGR resolves such references straight
    //    to the underlying type, losing the alias — so do the redirect ourselves.
    if let Some((word_start, word_end, word)) = cursor_span {
      if let Some(loc) = self.try_imported_alias(
        &text, &path, word_start, word_end, word, &index, &config,
      ) {
        return Ok(Some(GotoDefinitionResponse::Scalar(loc)));
      }
    }

    let node = index
      .identifier_at(&path, byte)
      .filter(|i| i.target_node_id != 0)
      .and_then(|i| index.node(i.target_node_id))
      .or_else(|| {
        let (_, _, word) = cursor_span?;
        index.find_node_by_short_name(word, &path)
      });
    let Some(node) = node else { return Ok(None) };
    if node.start_byte == 0 && node.end_byte == 0 {
      return Ok(None);
    }

    // Resolve target file: compiler reports it relative to its working dir; if the path
    // doesn't exist as-is, try relative to the requesting file's directory.
    let target_path =
      resolve_target_file(&node.file, &path, &config.resolution_roots);

    let target_uri = match Url::from_file_path(&target_path) {
      Ok(u) => u,
      Err(_) => {
        tracing::warn!("goto: bad target uri for {}", target_path.display());
        return Ok(None);
      }
    };
    let target_text = match std::fs::read_to_string(&target_path) {
      Ok(t) => t,
      Err(e) => {
        warn!("cannot read {}: {e}", target_path.display());
        return Ok(None);
      }
    };
    let target_rope = Rope::from_str(&target_text);
    // Prefer narrowing the range to just the declared name (so the editor lands on
    // `Foo` rather than the leading `struct` keyword). Fall back to the whole range
    // if we can't find the name token, e.g. an unusual declaration shape.
    let leaf_name = node
      .short_name
      .rsplit('.')
      .next()
      .unwrap_or(&node.short_name);
    let (start, end) = locate_decl_name(
      &target_text,
      node.start_byte as usize,
      node.end_byte as usize,
      leaf_name,
    )
    .map(|(s, e)| {
      (
        byte_to_position(&target_rope, s),
        byte_to_position(&target_rope, e),
      )
    })
    .unwrap_or_else(|| {
      (
        byte_to_position(&target_rope, node.start_byte as usize),
        byte_to_position(&target_rope, node.end_byte as usize),
      )
    });
    Ok(Some(GotoDefinitionResponse::Scalar(Location {
      uri: target_uri,
      range: Range { start, end },
    })))
  }

  async fn formatting(
    &self,
    params: DocumentFormattingParams,
  ) -> RpcResult<Option<Vec<TextEdit>>> {
    let uri = params.text_document.uri;
    let config = self.config.read().await.clone();
    if !config.format.enabled {
      return Ok(Some(Vec::new()));
    }
    let Some(text) = self.docs.get_text(&uri) else {
      return Ok(None);
    };
    let Some(out) = crate::format::format_document(&text, &config.format)
    else {
      return Ok(Some(Vec::new()));
    };
    // TODO: surface out.warnings via publishDiagnostics on a follow-up tick.
    let _ = out.warnings;
    if out.text == text {
      return Ok(Some(Vec::new()));
    }
    Ok(Some(line_diff_edits(&text, &out.text)))
  }

  async fn signature_help(
    &self,
    params: SignatureHelpParams,
  ) -> RpcResult<Option<SignatureHelp>> {
    let uri = params
      .text_document_position_params
      .text_document
      .uri
      .clone();
    let pos = params.text_document_position_params.position;
    let Some(text) = self.docs.get_text(&uri) else {
      return Ok(None);
    };
    let Some(index) = self.indices.get(&uri).map(|e| e.clone()) else {
      return Ok(None);
    };

    let rope = Rope::from_str(&text);
    let byte = position_to_byte(&rope, pos) as usize;

    // Find the unmatched `(` to the left of the cursor on the same logical expression
    // and the identifier (or dotted path) immediately preceding it.
    let Some(call) = enclosing_call(&text, byte) else {
      return Ok(None);
    };

    // Resolve the callee. Two cases:
    //   1. Builtin generic: `List` (one type param)
    //   2. Index lookup by leaf name: annotation -> use its value-struct's fields;
    //      struct/interface -> use its generic parameters.
    let signature = if call.callee == "List" {
      Some(SignatureInformation {
        label: "List(T)".into(),
        documentation: Some(Documentation::String(
          "List of T. Element type follows.".into(),
        )),
        parameters: Some(vec![ParameterInformation {
          label: ParameterLabel::Simple("T".into()),
          documentation: None,
        }]),
        active_parameter: Some(0),
      })
    } else {
      let leaf = call.callee.rsplit('.').next().unwrap_or(&call.callee);
      let path = uri.to_file_path().ok();
      let node = path
        .as_ref()
        .and_then(|p| index.find_node_by_short_name(leaf, p))
        .or_else(|| {
          index.nodes.values().find(|n| {
            let leaf_name =
              n.short_name.rsplit('.').next().unwrap_or(&n.short_name);
            leaf_name == leaf
          })
        });
      let Some(node) = node else { return Ok(None) };
      match node.kind {
        NodeKind::Annotation => {
          match node.annotation_value_type.and_then(|id| index.node(id)) {
            Some(value_node) if !value_node.fields.is_empty() => {
              Some(build_field_signature(
                &format!("${}", call.callee),
                &value_node.fields,
              ))
            }
            _ => None,
          }
        }
        NodeKind::Struct | NodeKind::Interface
          if !node.parameters.is_empty() =>
        {
          Some(build_generic_signature(&call.callee, &node.parameters))
        }
        _ => None,
      }
    };

    let Some(mut signature) = signature else {
      return Ok(None);
    };
    let n = signature.parameters.as_ref().map_or(0, |p| p.len()) as u32;
    let active = call.active_parameter.min(n.saturating_sub(1));
    signature.active_parameter = Some(active);
    Ok(Some(SignatureHelp {
      signatures: vec![signature],
      active_signature: Some(0),
      active_parameter: Some(active),
    }))
  }

  async fn semantic_tokens_full(
    &self,
    params: SemanticTokensParams,
  ) -> RpcResult<Option<SemanticTokensResult>> {
    let uri = params.text_document.uri;
    let Some(text) = self.docs.get_text(&uri) else {
      return Ok(None);
    };
    let data = semantic_tokens::full(&text);
    Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
      result_id: None,
      data,
    })))
  }

  async fn hover(&self, params: HoverParams) -> RpcResult<Option<Hover>> {
    let uri = params
      .text_document_position_params
      .text_document
      .uri
      .clone();
    let pos = params.text_document_position_params.position;
    let Some(text) = self.docs.get_text(&uri) else {
      return Ok(None);
    };
    let Ok(path) = uri.to_file_path() else {
      return Ok(None);
    };
    let Some(index) = self.indices.get(&uri).map(|e| e.clone()) else {
      return Ok(None);
    };

    let rope = Rope::from_str(&text);
    let byte = position_to_byte(&rope, pos);

    // Resolve to a node via the smallest containing FSI ident, then prefer the
    // member-component (longest containing ident ending at the cursor word) so that
    // for `Json.flatten` we hover the `flatten` annotation, not the `Json` file.
    let node = index
            .identifiers_at(&path, byte)
            .into_iter()
            .rev() // try longest first
            .find_map(|i| index.node(i.target_node_id))
            .or_else(|| {
                // FSI miss (e.g. type parameter inside `List(T)`): fall back to looking
                // up the cursor's identifier text in the index by name, preferring nodes
                // declared in the current file.
                let (_, _, word) = identifier_span_at_byte(&text, byte as usize)?;
                index.find_node_by_short_name(word, &path)
            });
    let Some(node) = node else { return Ok(None) };

    let mut md = String::new();
    let kind_label = match node.kind {
      NodeKind::Struct => "struct",
      NodeKind::Enum => "enum",
      NodeKind::Interface => "interface",
      NodeKind::Annotation => "annotation",
      NodeKind::Const => "const",
      NodeKind::File => "file",
      NodeKind::Other => "node",
    };
    let display = if node.short_name.is_empty() {
      node.display_name.clone()
    } else {
      node.short_name.clone()
    };
    md.push_str(&format!("```capnp\n{kind_label} {display}\n```\n"));
    if let Some(doc) = &node.doc_comment {
      md.push('\n');
      md.push_str(doc.trim_end());
      md.push('\n');
    }
    Ok(Some(Hover {
      contents: HoverContents::Markup(MarkupContent {
        kind: MarkupKind::Markdown,
        value: md,
      }),
      range: None,
    }))
  }

  async fn completion(
    &self,
    params: CompletionParams,
  ) -> RpcResult<Option<CompletionResponse>> {
    let uri = params.text_document_position.text_document.uri.clone();
    let pos = params.text_document_position.position;
    let Some(text) = self.docs.get_text(&uri) else {
      return Ok(None);
    };
    let Ok(path) = uri.to_file_path() else {
      return Ok(None);
    };
    let Some(index) = self.indices.get(&uri).map(|e| e.clone()) else {
      return Ok(None);
    };

    let rope = Rope::from_str(&text);
    let byte = position_to_byte(&rope, pos) as usize;
    let ctx = completion_context(&text, byte);

    // Built-in types and top-level keywords are always available regardless of whether
    // the file currently parses, so emit them up-front for the relevant slots.
    let mut prelude: Vec<CompletionItem> = Vec::new();
    match &ctx {
      CursorContext::Type => prelude.extend(builtin_type_items()),
      CursorContext::Unknown => prelude.extend(top_level_keyword_items()),
      _ => {}
    }

    // Collect the relevant subset of candidates given the cursor's slot.
    let candidates: Vec<&NodeInfo> = match &ctx {
      CursorContext::Type => index.type_candidates().collect(),
      CursorContext::Annotation => index.annotation_candidates().collect(),
      CursorContext::Member { namespace } => {
        // First try: `namespace` is a local named node (struct/interface/enum) whose
        // nested children we should offer — e.g. `Service.` -> `Kind`.
        let nested: Vec<&NodeInfo> = index
          .find_node_by_short_name(namespace, &path)
          .map(|p| index.nested_candidates(p.id))
          .unwrap_or_default();
        if !nested.is_empty() {
          nested
        } else if let Some(import_path) = aliases::import_path_for(&text, namespace) {
          let config = self.config.read().await.clone();
          let reported =
            std::path::PathBuf::from(import_path.trim_start_matches('/'));
          let target =
            resolve_target_file(&reported, &path, &config.resolution_roots);
          let from_index = index.candidates_in_file(&target);
          if !from_index.is_empty() {
            from_index
          } else if let Ok(target_text) = std::fs::read_to_string(&target) {
            // Imported file isn't in our CGR (nothing from it survived).
            // Fall back to a surface-text scan of its top-level declarations.
            return Ok(Some(CompletionResponse::Array(
              aliases::scan_top_level(&target_text)
                .into_iter()
                .map(|d| CompletionItem {
                  label: d.name,
                  kind: Some(match d.kind {
                    aliases::DeclKind::Struct
                    | aliases::DeclKind::Interface => {
                      CompletionItemKind::STRUCT
                    }
                    aliases::DeclKind::Enum => CompletionItemKind::ENUM,
                    aliases::DeclKind::Annotation => {
                      CompletionItemKind::INTERFACE
                    }
                    aliases::DeclKind::Const => CompletionItemKind::CONSTANT,
                    aliases::DeclKind::Using => {
                      CompletionItemKind::TYPE_PARAMETER
                    }
                  }),
                  detail: Some(format!("from {}", target.display())),
                  documentation: d.doc_comment.map(|d| {
                    Documentation::MarkupContent(MarkupContent {
                      kind: MarkupKind::Markdown,
                      value: d,
                    })
                  }),
                  ..Default::default()
                })
                .collect(),
            )));
          } else {
            Vec::new()
          }
        } else {
          Vec::new()
        }
      }
      CursorContext::FieldOrdinal => {
        // Inside a struct/enum's body: offer every valid ordinal — gaps in the
        // existing sequence first (handy when a field was deleted earlier), then
        // the next-after-max. Outside any struct/enum: generate a fresh capnp ID.
        let candidates = ordinals::suggest_ordinals_at(&text, byte);
        if !candidates.is_empty() {
          let items: Vec<CompletionItem> = candidates
            .iter()
            .enumerate()
            .map(|(rank, n)| CompletionItem {
              label: n.to_string(),
              kind: Some(CompletionItemKind::VALUE),
              detail: Some(if rank == 0 && candidates.len() > 1 {
                "next available ordinal (fills a gap)".to_string()
              } else if rank == 0 {
                "next field ordinal".to_string()
              } else {
                format!("valid ordinal (rank {})", rank + 1)
              }),
              // sort_text keeps our order: 0000_{rank} so the first is preselected.
              sort_text: Some(format!("{:04}_{:08}", rank, n)),
              preselect: Some(rank == 0),
              ..Default::default()
            })
            .collect();
          return Ok(Some(CompletionResponse::Array(items)));
        }
        // Top-level / declaration site: generate a unique capnp ID.
        let config = self.config.read().await.clone();
        if let Some(id) = generate_capnp_id(&config.compiler_path).await {
          // The user already typed the `@`, so insert just `0x...`.
          let bare = id.trim_start_matches('@').to_string();
          return Ok(Some(CompletionResponse::Array(vec![CompletionItem {
            label: id.clone(),
            kind: Some(CompletionItemKind::VALUE),
            detail: Some("freshly generated capnp ID".to_string()),
            insert_text: Some(bare),
            sort_text: Some("0000_id".to_string()),
            preselect: Some(true),
            ..Default::default()
          }])));
        }
        return Ok(None);
      }
      CursorContext::Unknown => index.completion_candidates().collect(),
      CursorContext::None => return Ok(None),
    };

    let mut items: Vec<CompletionItem> = prelude;
    items.extend(candidates.into_iter().map(|n| CompletionItem {
      label: n.short_name.clone(),
      kind: Some(match n.kind {
        NodeKind::Struct | NodeKind::Interface => CompletionItemKind::STRUCT,
        NodeKind::Enum => CompletionItemKind::ENUM,
        NodeKind::Annotation => CompletionItemKind::INTERFACE,
        NodeKind::Const => CompletionItemKind::CONSTANT,
        _ => CompletionItemKind::TEXT,
      }),
      detail: Some(n.display_name.clone()),
      documentation: n.doc_comment.as_ref().map(|d| {
        Documentation::MarkupContent(MarkupContent {
          kind: MarkupKind::Markdown,
          value: d.clone(),
        })
      }),
      ..Default::default()
    }));
    Ok(Some(CompletionResponse::Array(items)))
  }
}

/// Within `text[start..end]`, find the byte range of the first occurrence of `name` as a
/// whole identifier token. Used to narrow a node's whole-declaration range down to just
/// its name (so goto lands on `Foo` rather than the `struct` keyword).
fn locate_decl_name(
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

/// Map a compiler-reported file path back to a real on-disk path. The capnp compiler
/// normalizes absolute paths by stripping the leading `/`, so a reported "Users/foo/bar.capnp"
/// is really "/Users/foo/bar.capnp". Imported standard files like "capnp/compat/json.capnp"
/// live under the install's include dir, which `roots` covers.
fn resolve_target_file(
  reported: &Path,
  requesting: &Path,
  roots: &[PathBuf],
) -> PathBuf {
  if reported.is_absolute() && reported.exists() {
    return reported.to_path_buf();
  }
  let with_slash = PathBuf::from(format!("/{}", reported.display()));
  if with_slash.exists() {
    return with_slash;
  }
  if let Some(parent) = requesting.parent() {
    let candidate = parent.join(reported);
    if candidate.exists() {
      return candidate;
    }
  }
  for root in roots {
    let candidate = root.join(reported);
    if candidate.exists() {
      return candidate;
    }
  }
  reported.to_path_buf()
}

/// A call (annotation application or generic instantiation) found by walking back from
/// the cursor. `callee` is the dotted name (e.g. `Json.discriminator`, `List`, or `Map`),
/// `active_parameter` is the comma index of the cursor inside the parens.
struct EnclosingCall {
  callee: String,
  active_parameter: u32,
}

fn enclosing_call(text: &str, cursor: usize) -> Option<EnclosingCall> {
  let bytes = text.as_bytes();
  if cursor > bytes.len() {
    return None;
  }
  // Walk back tracking paren depth; stop at the unmatched `(` that contains us.
  let mut depth: i32 = 0;
  let mut commas: u32 = 0;
  let mut i = cursor;
  while i > 0 {
    i -= 1;
    match bytes[i] {
      b')' | b']' | b'}' => depth += 1,
      b'(' | b'[' | b'{' => {
        depth -= 1;
        if depth < 0 && bytes[i] == b'(' {
          // Found our unmatched `(` at byte i. Identify the callee just before.
          let mut j = i;
          while j > 0 && bytes[j - 1].is_ascii_whitespace() {
            j -= 1;
          }
          let mut k = j;
          while k > 0
            && (bytes[k - 1].is_ascii_alphanumeric()
              || bytes[k - 1] == b'_'
              || bytes[k - 1] == b'.')
          {
            k -= 1;
          }
          if k == j {
            return None;
          }
          let callee = std::str::from_utf8(&bytes[k..j]).ok()?.to_string();
          return Some(EnclosingCall {
            callee,
            active_parameter: commas,
          });
        }
        if depth < 0 {
          return None; // unmatched `[` or `{` — not an annotation/call
        }
      }
      b',' if depth == 0 => commas += 1,
      _ => {}
    }
  }
  None
}

fn build_field_signature(
  label_prefix: &str,
  fields: &[crate::index::FieldInfo],
) -> SignatureInformation {
  let mut label = String::from(label_prefix);
  label.push('(');
  let mut params: Vec<ParameterInformation> = Vec::with_capacity(fields.len());
  for (i, f) in fields.iter().enumerate() {
    if i > 0 {
      label.push_str(", ");
    }
    let start = label.len() as u32;
    label.push_str(&f.name);
    label.push_str(" = ");
    label.push_str(&f.type_str);
    let end = label.len() as u32;
    params.push(ParameterInformation {
      label: ParameterLabel::LabelOffsets([start, end]),
      documentation: None,
    });
  }
  label.push(')');
  SignatureInformation {
    label,
    documentation: None,
    parameters: Some(params),
    active_parameter: None,
  }
}

fn build_generic_signature(
  callee: &str,
  params: &[String],
) -> SignatureInformation {
  let mut label = String::from(callee);
  label.push('(');
  let mut out: Vec<ParameterInformation> = Vec::with_capacity(params.len());
  for (i, p) in params.iter().enumerate() {
    if i > 0 {
      label.push_str(", ");
    }
    let start = label.len() as u32;
    label.push_str(p);
    let end = label.len() as u32;
    out.push(ParameterInformation {
      label: ParameterLabel::LabelOffsets([start, end]),
      documentation: None,
    });
  }
  label.push(')');
  SignatureInformation {
    label,
    documentation: None,
    parameters: Some(out),
    active_parameter: None,
  }
}

/// Run `capnp id` and return the generated `@0x...` token, or None if the binary
/// fails for any reason.
async fn generate_capnp_id(compiler: &str) -> Option<String> {
  let output = tokio::process::Command::new(compiler)
    .arg("id")
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::null())
    .output()
    .await
    .ok()?;
  if !output.status.success() {
    return None;
  }
  let s = std::str::from_utf8(&output.stdout).ok()?.trim();
  s.starts_with("@0x").then(|| s.to_string())
}

/// Compute a minimal set of `TextEdit`s that transform `old` into `new` by diffing line
/// by line and returning one TextEdit per contiguous run of changed lines. Keeps the
/// editor's cursor stable in unchanged regions instead of yanking it to the start the
/// way a single full-document edit does.
fn line_diff_edits(old: &str, new: &str) -> Vec<TextEdit> {
  use similar::{ChangeTag, TextDiff};

  let diff = TextDiff::configure()
    .algorithm(similar::Algorithm::Myers)
    .diff_lines(old, new);

  let mut edits: Vec<TextEdit> = Vec::new();
  // Walk grouped operations. Each non-Equal `ChangeTag` run becomes one TextEdit
  // covering the deleted (old) line range, replaced by the inserted (new) text.
  let mut old_line: u32 = 0; // 0-based line in `old`
  let mut pending_delete_start: Option<u32> = None;
  let mut pending_delete_end: u32 = 0;
  let mut pending_insert_text = String::new();

  let flush = |start: Option<u32>,
               end: u32,
               text: &mut String,
               edits: &mut Vec<TextEdit>| {
    let Some(s) = start else { return };
    let new_text = std::mem::take(text);
    edits.push(TextEdit {
      range: Range {
        start: Position::new(s, 0),
        end: Position::new(end, 0),
      },
      new_text,
    });
  };

  for change in diff.iter_all_changes() {
    match change.tag() {
      ChangeTag::Equal => {
        // Close out any pending delete/insert run.
        flush(
          pending_delete_start.take(),
          pending_delete_end,
          &mut pending_insert_text,
          &mut edits,
        );
        old_line += 1;
      }
      ChangeTag::Delete => {
        if pending_delete_start.is_none() {
          pending_delete_start = Some(old_line);
        }
        pending_delete_end = old_line + 1;
        old_line += 1;
      }
      ChangeTag::Insert => {
        if pending_delete_start.is_none() {
          // Pure insert (no surrounding delete): an empty range at the current
          // old-line position.
          pending_delete_start = Some(old_line);
          pending_delete_end = old_line;
        }
        pending_insert_text.push_str(change.value());
      }
    }
  }
  flush(
    pending_delete_start,
    pending_delete_end,
    &mut pending_insert_text,
    &mut edits,
  );
  edits
}

/// Cap'n Proto's built-in primitive types, plus the parametric ones. Always offered in
/// type-slot completion so they're available even on a buffer that doesn't currently
/// parse (a CGR-empty index can't supply them).
const BUILTIN_TYPES: &[&str] = &[
  "Void",
  "Bool",
  "Int8",
  "Int16",
  "Int32",
  "Int64",
  "UInt8",
  "UInt16",
  "UInt32",
  "UInt64",
  "Float32",
  "Float64",
  "Text",
  "Data",
  "List",
  "AnyPointer",
  "AnyStruct",
  "Capability",
];

fn builtin_type_items() -> impl IntoIterator<Item = CompletionItem> {
  BUILTIN_TYPES.iter().map(|name| CompletionItem {
    label: name.to_string(),
    kind: Some(CompletionItemKind::KEYWORD),
    detail: Some("built-in type".to_string()),
    sort_text: Some(format!("0_{name}")), // float to top
    ..Default::default()
  })
}

/// Top-level / declaration-introducing keywords. Offered when we don't otherwise know
/// what the cursor expects.
const TOP_LEVEL_KEYWORDS: &[&str] = &[
  "struct",
  "enum",
  "interface",
  "union",
  "group",
  "using",
  "import",
  "const",
  "annotation",
  "extends",
];

fn top_level_keyword_items() -> impl IntoIterator<Item = CompletionItem> {
  TOP_LEVEL_KEYWORDS.iter().map(|kw| CompletionItem {
    label: kw.to_string(),
    kind: Some(CompletionItemKind::KEYWORD),
    sort_text: Some(format!("0_{kw}")),
    ..Default::default()
  })
}

/// What kind of identifier the cursor is positioned to receive. Used to filter
/// completion candidates to the relevant subset.
#[derive(Debug)]
enum CursorContext<'a> {
  /// After `:` or `(` or `,` — a type slot (struct/enum/interface/const).
  Type,
  /// After `$` — an annotation slot.
  Annotation,
  /// After `Namespace.` — a member of an imported file.
  Member { namespace: &'a str },
  /// After `@` (optionally followed by digits being typed) — suggest the next field
  /// ordinal in the enclosing struct's ID space.
  FieldOrdinal,
  /// We can't tell — return everything (preserve old behaviour).
  Unknown,
  /// Definitely not a completion site (inside a comment or string).
  None,
}

fn completion_context(text: &str, cursor: usize) -> CursorContext<'_> {
  let bytes = text.as_bytes();
  if cursor > bytes.len() {
    return CursorContext::None;
  }
  // Bail out if cursor sits inside a comment or a string literal on the current line.
  let line_start = bytes[..cursor]
    .iter()
    .rposition(|&b| b == b'\n')
    .map_or(0, |i| i + 1);
  let line_so_far = &text[line_start..cursor];
  if line_so_far.contains('#') {
    return CursorContext::None;
  }
  let mut quotes = 0;
  for &b in line_so_far.as_bytes() {
    if b == b'"' {
      quotes += 1;
    }
  }
  if quotes % 2 == 1 {
    return CursorContext::None;
  }
  // Field-ordinal completion: cursor is right after `@` (no digits yet) or in the
  // middle of typing the digits after `@`.
  {
    let mut k = cursor;
    while k > 0 && bytes[k - 1].is_ascii_digit() {
      k -= 1;
    }
    if k > 0 && bytes[k - 1] == b'@' {
      return CursorContext::FieldOrdinal;
    }
  }
  // Skip the identifier currently being typed.
  let mut i = cursor;
  while i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_')
  {
    i -= 1;
  }
  let word_start = i;
  // Skip preceding whitespace (within the same logical line — but capnp continues
  // type expressions across newlines, so allow newlines too).
  while i > 0 && bytes[i - 1].is_ascii_whitespace() {
    i -= 1;
  }
  if i == 0 {
    return CursorContext::Unknown;
  }
  let _ = word_start;
  match bytes[i - 1] {
    b':' => CursorContext::Type,
    b'$' => CursorContext::Annotation,
    b'(' | b',' => CursorContext::Type,
    b'.' => {
      let dot = i - 1;
      let mut ns_start = dot;
      while ns_start > 0
        && (bytes[ns_start - 1].is_ascii_alphanumeric()
          || bytes[ns_start - 1] == b'_')
      {
        ns_start -= 1;
      }
      if ns_start < dot {
        CursorContext::Member {
          namespace: &text[ns_start..dot],
        }
      } else {
        CursorContext::Unknown
      }
    }
    _ => CursorContext::Unknown,
  }
}

/// If `byte` falls inside a `"..."` string that's the operand of an `import` keyword,
/// return the contents of the string. capnp imports are always plain double-quoted ASCII
/// paths, so a byte-level scan is sufficient — no need for tree-sitter.
fn import_string_at_byte(text: &str, byte: usize) -> Option<&str> {
  let bytes = text.as_bytes();
  if byte > bytes.len() {
    return None;
  }
  // Find the opening quote on the same line.
  let line_start = bytes[..byte]
    .iter()
    .rposition(|&b| b == b'\n')
    .map_or(0, |i| i + 1);
  let mut open = None;
  let mut i = line_start;
  while i < byte {
    if bytes[i] == b'"' {
      open = Some(i);
      // Skip to closing quote (capnp doesn't use escapes in import paths).
      i += 1;
      while i < bytes.len() && bytes[i] != b'"' && bytes[i] != b'\n' {
        i += 1;
      }
      if i < bytes.len() && bytes[i] == b'"' {
        i += 1;
        if i > byte {
          break;
        }
        open = None;
      }
    } else {
      i += 1;
    }
  }
  let open = open?;
  // Find matching close after `byte`.
  let close =
    (open + 1..bytes.len()).find(|&j| bytes[j] == b'"' || bytes[j] == b'\n')?;
  if bytes[close] != b'"' || byte < open + 1 || byte > close {
    return None;
  }
  // Verify the preceding non-whitespace token (on the same line) is `import`.
  let before = &text[line_start..open];
  let trimmed = before.trim_end();
  if !trimmed.ends_with("import") {
    return None;
  }
  // And the char before `import` is a word boundary.
  let kw_start = trimmed.len() - "import".len();
  if kw_start > 0 {
    let prev = trimmed.as_bytes()[kw_start - 1];
    if prev.is_ascii_alphanumeric() || prev == b'_' {
      return None;
    }
  }
  std::str::from_utf8(&bytes[open + 1..close]).ok()
}

/// Extract the identifier slice at `byte` along with its byte span, if the cursor sits
/// inside one.
fn identifier_span_at_byte(
  text: &str,
  byte: usize,
) -> Option<(usize, usize, &str)> {
  let bytes = text.as_bytes();
  if byte > bytes.len() {
    return None;
  }
  let is_ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
  let mut start = byte;
  while start > 0 && is_ident(bytes[start - 1]) {
    start -= 1;
  }
  let mut end = byte;
  while end < bytes.len() && is_ident(bytes[end]) {
    end += 1;
  }
  if start == end
    || (!bytes[start].is_ascii_alphabetic() && bytes[start] != b'_')
  {
    return None;
  }
  Some((start, end, std::str::from_utf8(&bytes[start..end]).ok()?))
}

fn position_to_byte(rope: &Rope, pos: Position) -> u32 {
  let line = (pos.line as usize).min(rope.len_lines().saturating_sub(1));
  let line_start_char = rope.line_to_char(line);
  let line_slice = rope.line(line);
  let col = (pos.character as usize).min(line_slice.len_chars());
  let char_idx = line_start_char + col;
  rope.char_to_byte(char_idx) as u32
}

fn byte_to_position(rope: &Rope, byte: usize) -> Position {
  let byte = byte.min(rope.len_bytes());
  let char_idx = rope.byte_to_char(byte);
  let line = rope.char_to_line(char_idx);
  let line_start = rope.line_to_char(line);
  Position::new(line as u32, (char_idx - line_start) as u32)
}
