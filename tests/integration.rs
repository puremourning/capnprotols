//! Integration tests for the capnprotols LSP server.
//!
//! Each test spins up a real binary, drives it through the LSP protocol over stdio, and
//! asserts on the JSON responses. Tests share a small client harness in `common/`.
//!
//! `cargo test --test integration` runs them.

mod common;

use std::process::Command;

use serde_json::{json, Value};

use crate::common::{LspClient, TempProject};

/// Build a TempProject that contains user.capnp + types.capnp (since user imports types).
fn user_project() -> TempProject {
  TempProject::with_fixtures(&["user.capnp", "types.capnp"])
}

/// Find the column right after the first occurrence of `needle` on lines that contain
/// `line_match`. Returns (line, column).
fn locate(text: &str, line_match: &str, needle: &str) -> (u32, u32) {
  for (i, line) in text.lines().enumerate() {
    if line.contains(line_match) {
      if let Some(c) = line.find(needle) {
        return (i as u32, (c + needle.len()) as u32);
      }
    }
  }
  panic!("locate: line containing {line_match:?} with {needle:?} not found");
}

/// Position the cursor inside `needle` (one byte past its start) on the first line that
/// contains `line_match`.
fn locate_inside(text: &str, line_match: &str, needle: &str) -> (u32, u32) {
  for (i, line) in text.lines().enumerate() {
    if line.contains(line_match) {
      if let Some(c) = line.find(needle) {
        return (i as u32, (c + 1) as u32);
      }
    }
  }
  panic!(
    "locate_inside: line containing {line_match:?} with {needle:?} not found"
  );
}

fn pos(line: u32, character: u32) -> Value {
  json!({ "line": line, "character": character })
}

#[test]
fn initialize_advertises_capabilities() {
  let mut c = LspClient::start();
  // initialize already happened; ask for the result by triggering one method that
  // requires a capability and checking it doesn't error.
  let r = c.request_no_params("shutdown");
  assert!(r.get("error").is_none(), "shutdown error: {r}");
  c.notify_no_params("exit");
}

#[test]
fn diagnostics_published_on_open() {
  let mut c = LspClient::start();
  let proj = user_project();
  let diags = c.open(&proj.uri("user.capnp"), &proj.text("user.capnp"));
  assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
  c.shutdown();
}

#[test]
fn diagnostics_have_ranges_and_messages() {
  let mut c = LspClient::start();
  let proj = TempProject::with_fixtures(&[]);
  let path = proj.path("bad.capnp");
  let text = "@0xeaf06436acd04fc9;\nstruct Foo { foo @0 :NoSuchType; }\n";
  std::fs::write(&path, text).unwrap();
  let uri = format!("file://{}", path.display());

  let diags = c.open(&uri, text);
  assert!(!diags.is_empty(), "expected diagnostics");
  let d = &diags[0];
  assert_eq!(d["source"], "capnp");
  let msg = d["message"].as_str().unwrap();
  assert!(msg.contains("NoSuchType"), "msg: {msg}");
  let range = &d["range"];
  assert_eq!(
    range["start"]["line"], 1,
    "should land on line 2 (0-indexed 1)"
  );
  let start_char = range["start"]["character"].as_u64().unwrap();
  let end_char = range["end"]["character"].as_u64().unwrap();
  assert!(
    end_char > start_char,
    "expected non-empty range, got {start_char}..{end_char}"
  );
  c.shutdown();
}

#[test]
fn diagnostics_report_syntax_errors() {
  let mut c = LspClient::start();
  let proj = user_project();
  let uri = proj.uri("user.capnp");
  let text = proj.text("user.capnp");
  c.open(&uri, &text);

  let broken = format!("{}\nGARBAGE_TOKEN!!!\n", text);
  let diags = c.change(&uri, 2, &broken);
  assert!(!diags.is_empty(), "expected diagnostics for broken file");
  c.shutdown();
}

#[test]
fn goto_definition_resolves_imported_alias() {
  let mut c = LspClient::start();
  let proj = user_project();
  let uri = proj.uri("user.capnp");
  let text = proj.text("user.capnp");
  c.open(&uri, &text);

  // Cursor inside `UUID` of `Types.UUID` on the organisationId line.
  let (line, col) =
    locate_inside(&text, "organisationId @0 :Types.UUID", "UUID");
  let r = c.request(
    "textDocument/definition",
    json!({ "textDocument": { "uri": uri }, "position": pos(line, col) }),
  );
  let result = &r["result"];
  assert!(result.is_object(), "expected definition result, got {r}");
  let target = result["uri"].as_str().expect("target uri");
  assert!(target.ends_with("/types.capnp"), "got {target}");
  let line0 = result["range"]["start"]["line"].as_u64().unwrap();
  assert!(
    line0 < 5,
    "should land on the `using UUID` line near top, got {line0}"
  );
  c.shutdown();
}

#[test]
fn goto_definition_resolves_local_alias() {
  let mut c = LspClient::start();
  let proj = user_project();
  let uri = proj.uri("user.capnp");
  let text = proj.text("user.capnp");
  c.open(&uri, &text);

  // Cursor inside `Types` of `Types.UUID`.
  let (line, col) =
    locate_inside(&text, "organisationId @0 :Types.UUID", "Types");
  let r = c.request(
    "textDocument/definition",
    json!({ "textDocument": { "uri": uri }, "position": pos(line, col) }),
  );
  let result = &r["result"];
  let target = result["uri"].as_str().expect("uri");
  assert!(target.ends_with("/user.capnp"));
  let target_line = result["range"]["start"]["line"].as_u64().unwrap();
  assert_eq!(target_line, 2, "should land on `using Types = ...`");
  c.shutdown();
}

#[test]
fn goto_definition_falls_back_for_nested_type_in_generic() {
  // The type parameter of `List(Inner)` is a nested struct — capnp's FSI doesn't
  // record the inner position, so this exercises the name-based fallback resolving
  // a nested (dotted displayName) target.
  let mut c = LspClient::start();
  let proj = user_project();
  let uri = proj.uri("user.capnp");
  let text = proj.text("user.capnp");
  c.open(&uri, &text);

  let (line, col) = locate_inside(&text, "List(Inner)", "Inner");
  let r = c.request(
    "textDocument/definition",
    json!({ "textDocument": { "uri": uri }, "position": pos(line, col) }),
  );
  let result = &r["result"];
  assert!(result.is_object(), "expected definition, got {r}");
  let target = result["uri"].as_str().unwrap();
  assert!(target.ends_with("/user.capnp"), "got {target}");
  let target_text = std::fs::read_to_string(&proj.path("user.capnp")).unwrap();
  let target_line = result["range"]["start"]["line"].as_u64().unwrap() as usize;
  let decl = target_text.lines().nth(target_line).unwrap_or("");
  assert!(
    decl.contains("struct Inner"),
    "expected to land on `struct Inner`, got line: {:?}",
    decl
  );
  c.shutdown();
}

#[test]
fn goto_definition_for_self_nested_in_generic() {
  // Mirrors the real-world case: `struct UserLike { struct SamlIdentity {...};
  // samlIdentities @0 :List(SamlIdentity); }` — the cursor on SamlIdentity inside
  // the List should land on the nested struct declaration.
  let mut c = LspClient::start();
  let proj = user_project();
  let uri = proj.uri("user.capnp");
  let text = proj.text("user.capnp");
  c.open(&uri, &text);

  let (line, col) = locate_inside(&text, "List(SamlIdentity)", "SamlIdentity");
  let r = c.request(
    "textDocument/definition",
    json!({ "textDocument": { "uri": uri }, "position": pos(line, col) }),
  );
  let result = &r["result"];
  assert!(result.is_object(), "expected definition, got {r}");
  let target_line = result["range"]["start"]["line"].as_u64().unwrap() as usize;
  let target_text = std::fs::read_to_string(&proj.path("user.capnp")).unwrap();
  let decl = target_text.lines().nth(target_line).unwrap_or("");
  assert!(
    decl.contains("struct SamlIdentity"),
    "expected `struct SamlIdentity`, got {:?}",
    decl
  );
  // Range should point at the name `SamlIdentity` itself, not the `struct` keyword.
  let start_col =
    result["range"]["start"]["character"].as_u64().unwrap() as usize;
  let end_col = result["range"]["end"]["character"].as_u64().unwrap() as usize;
  assert_eq!(
    &decl[start_col..end_col],
    "SamlIdentity",
    "expected target range to span the name token; line was {decl:?}"
  );
  c.shutdown();
}

#[test]
fn goto_definition_falls_back_for_generic_parameters() {
  // CGR's FSI has no entry for `AuthToken` inside `List(AuthToken)`, so we exercise the
  // name-based fallback.
  let mut c = LspClient::start();
  let proj = user_project();
  let uri = proj.uri("user.capnp");
  let text = proj.text("user.capnp");
  c.open(&uri, &text);

  let (line, col) = locate_inside(&text, "List(AuthToken)", "AuthToken");
  let r = c.request(
    "textDocument/definition",
    json!({ "textDocument": { "uri": uri }, "position": pos(line, col) }),
  );
  let result = &r["result"];
  assert!(result.is_object(), "expected definition, got {r}");
  let target = result["uri"].as_str().unwrap();
  assert!(target.ends_with("/user.capnp"));
  c.shutdown();
}

#[test]
fn hover_returns_doc_comment() {
  let mut c = LspClient::start();
  let proj = user_project();
  let uri = proj.uri("user.capnp");
  let text = proj.text("user.capnp");
  c.open(&uri, &text);

  // Hover on AuthToken in `List(AuthToken)` to exercise hover via name fallback.
  let (line, col) = locate_inside(&text, "List(AuthToken)", "AuthToken");
  let r = c.request(
    "textDocument/hover",
    json!({ "textDocument": { "uri": uri }, "position": pos(line, col) }),
  );
  let value = r["result"]["contents"]["value"]
    .as_str()
    .expect("hover markup");
  assert!(value.contains("AuthToken"), "hover label missing: {value}");
  assert!(
    value.contains("Opaque session token"),
    "doc comment missing: {value}"
  );
  c.shutdown();
}

#[test]
fn completion_after_colon_includes_builtins_and_user_types() {
  let mut c = LspClient::start();
  let proj = user_project();
  let uri = proj.uri("user.capnp");
  let text = proj.text("user.capnp");
  c.open(&uri, &text);

  // Right after `:` in `organisationId @0 :Types.UUID`.
  let (line, col) = locate(&text, "organisationId @0 :Types.UUID", ":");
  let r = c.request(
    "textDocument/completion",
    json!({ "textDocument": { "uri": uri }, "position": pos(line, col) }),
  );
  let items = r["result"].as_array().expect("array of items");
  let labels: Vec<&str> =
    items.iter().map(|i| i["label"].as_str().unwrap()).collect();
  for builtin in ["Text", "UInt32", "Bool", "List"] {
    assert!(
      labels.contains(&builtin),
      "missing builtin {builtin}: {labels:?}"
    );
  }
  assert!(labels.contains(&"AuthToken"), "missing user type AuthToken");
  c.shutdown();
}

#[test]
fn completion_after_dollar_only_annotations() {
  let mut c = LspClient::start();
  let proj = user_project();
  let uri = proj.uri("user.capnp");
  let text = proj.text("user.capnp");
  c.open(&uri, &text);

  let (line, col) = locate(&text, "$Json.hex", "$");
  let r = c.request(
    "textDocument/completion",
    json!({ "textDocument": { "uri": uri }, "position": pos(line, col) }),
  );
  let items = r["result"].as_array().expect("items");
  let labels: Vec<&str> =
    items.iter().map(|i| i["label"].as_str().unwrap()).collect();
  // pii is a local annotation; hex/base64 come from json.capnp.
  assert!(labels.contains(&"pii"), "want pii, got {labels:?}");
  assert!(labels.contains(&"hex"), "want hex");
  // No built-in primitives in annotation slot.
  assert!(!labels.contains(&"Text"));
  assert!(!labels.contains(&"UInt32"));
  c.shutdown();
}

#[test]
fn completion_after_dotted_namespace() {
  let mut c = LspClient::start();
  let proj = user_project();
  let uri = proj.uri("user.capnp");
  let text = proj.text("user.capnp");
  c.open(&uri, &text);

  let (line, col) = locate(&text, "organisationId @0 :Types.UUID", "Types.");
  let r = c.request(
    "textDocument/completion",
    json!({ "textDocument": { "uri": uri }, "position": pos(line, col) }),
  );
  let items = r["result"].as_array().expect("items");
  let labels: Vec<&str> =
    items.iter().map(|i| i["label"].as_str().unwrap()).collect();
  for want in ["UUID", "UTCSecondsSinceEpoch", "Side", "Date"] {
    assert!(labels.contains(&want), "want {want}, got {labels:?}");
  }
  c.shutdown();
}

#[test]
fn completion_field_ordinal_sequence() {
  let mut c = LspClient::start();
  let uri = "file:///tmp/capnprotols-test-ord.capnp".to_string();
  let text = "@0xeaf06436acd04fc5;\nstruct A {\n  foo @0 :Text;\n  bar @1 :UInt8;\n  baz @\n}\n";
  std::fs::write("/tmp/capnprotols-test-ord.capnp", text).unwrap();
  c.open(&uri, text);

  let r = c.request(
    "textDocument/completion",
    json!({ "textDocument": { "uri": uri }, "position": pos(4, 7) }),
  );
  let items = r["result"].as_array().expect("items");
  // Dense sequence (0, 1) -> only one candidate: the next-after-max.
  assert_eq!(items.len(), 1);
  assert_eq!(items[0]["label"], "2");
  assert_eq!(items[0]["detail"], "next field ordinal");
  c.shutdown();
}

#[test]
fn completion_field_ordinal_offers_gaps_first() {
  let mut c = LspClient::start();
  let uri = "file:///tmp/capnprotols-test-ord-gap.capnp".to_string();
  // Ordinals present: 0, 2, 3, 5. Gaps: 1, 4. Next: 6.
  let text = "@0xeaf06436acd04fca;\nstruct A {\n  a @0 :Text;\n  c @2 :Text;\n  d @3 :Text;\n  e @5 :Text;\n  f @\n}\n";
  std::fs::write("/tmp/capnprotols-test-ord-gap.capnp", text).unwrap();
  c.open(&uri, text);

  let r = c.request(
    "textDocument/completion",
    json!({ "textDocument": { "uri": uri }, "position": pos(6, 5) }),
  );
  let items = r["result"].as_array().expect("items");
  let labels: Vec<&str> =
    items.iter().map(|i| i["label"].as_str().unwrap()).collect();
  assert_eq!(labels, vec!["1", "4", "6"], "got {labels:?}");
  assert_eq!(items[0]["preselect"], true);
  c.shutdown();
}

#[test]
fn completion_top_level_at_generates_capnp_id() {
  let mut c = LspClient::start();
  let uri = "file:///tmp/capnprotols-test-id.capnp".to_string();
  let text = "@\n";
  std::fs::write("/tmp/capnprotols-test-id.capnp", text).unwrap();
  c.open(&uri, text);

  let r = c.request(
    "textDocument/completion",
    json!({ "textDocument": { "uri": uri }, "position": pos(0, 1) }),
  );
  let items = r["result"].as_array().expect("items");
  assert_eq!(items.len(), 1);
  let label = items[0]["label"].as_str().unwrap();
  assert!(
    label.starts_with("@0x") && label.len() == 19,
    "expected @0x... id, got {label}"
  );
  assert_eq!(items[0]["detail"], "freshly generated capnp ID");
  c.shutdown();
}

#[test]
fn signature_help_for_annotation() {
  let mut c = LspClient::start();
  let uri = "file:///tmp/capnprotols-test-sig.capnp".to_string();
  let text = "@0xeaf06436acd04fc6;\nusing Json = import \"/capnp/compat/json.capnp\";\nstruct Foo $Json.discriminator() {}\n";
  std::fs::write("/tmp/capnprotols-test-sig.capnp", text).unwrap();
  c.open(&uri, text);

  // Cursor right after `(` in `discriminator(`.
  let lines: Vec<&str> = text.lines().collect();
  let col = lines[2].find("discriminator(").unwrap() + "discriminator(".len();
  let r = c.request(
    "textDocument/signatureHelp",
    json!({ "textDocument": { "uri": uri }, "position": pos(2, col as u32) }),
  );
  let sig = &r["result"]["signatures"][0];
  let label = sig["label"].as_str().unwrap();
  assert!(label.contains("name"), "label missing `name`: {label}");
  assert!(label.contains(":Text"), "label missing :Text: {label}");
  assert_eq!(r["result"]["activeParameter"], 0);
  c.shutdown();
}

#[test]
fn signature_help_for_list() {
  let mut c = LspClient::start();
  let uri = "file:///tmp/capnprotols-test-list.capnp".to_string();
  let text = "@0xeaf06436acd04fc7;\nstruct A { xs @0 :List() ; }\n";
  std::fs::write("/tmp/capnprotols-test-list.capnp", text).unwrap();
  c.open(&uri, text);

  let col = text.lines().nth(1).unwrap().find("List(").unwrap() + "List(".len();
  let r = c.request(
    "textDocument/signatureHelp",
    json!({ "textDocument": { "uri": uri }, "position": pos(1, col as u32) }),
  );
  let sig = &r["result"]["signatures"][0];
  assert_eq!(sig["label"], "List(T)");
  c.shutdown();
}

#[test]
fn formatting_emits_minimal_per_line_edits() {
  // Only one line is dirty; the formatter should return a TextEdit covering just that
  // line range, not the whole file. This keeps editor cursors stable on save-format.
  let mut c = LspClient::start();
  let proj = TempProject::with_fixtures(&[]);
  let path = proj.path("partial.capnp");
  // Lines 1, 2, 4, 5 are clean; line 3 has the bad indent.
  let dirty = "@0xeaf06436acd04fdd;\nstruct A {\n        foo @0 :Text;\n  bar @1 :UInt8;\n}\n";
  std::fs::write(&path, dirty).unwrap();
  let uri = format!("file://{}", path.display());
  c.open(&uri, dirty);

  let r = c.request(
    "textDocument/formatting",
    json!({
        "textDocument": { "uri": uri },
        "options": { "tabSize": 2, "insertSpaces": true },
    }),
  );
  let edits = r["result"].as_array().expect("array of edits");
  assert!(!edits.is_empty(), "expected at least one edit");
  // No edit should cover the whole document. The first clean line is line 0
  // (`@0x...;`), so a 0-length edit at start is fine, but a single edit spanning
  // line 0 to past line 3 would mean we did a full-doc rewrite.
  for edit in edits {
    let start_line = edit["range"]["start"]["line"].as_u64().unwrap();
    let end_line = edit["range"]["end"]["line"].as_u64().unwrap();
    assert!(
      !(start_line == 0 && end_line >= 4),
      "edit covers the whole document: {edit:?}"
    );
  }
  c.shutdown();
}

#[test]
fn formatting_returns_text_edit_for_dirty_file() {
  let mut c = LspClient::start();
  let proj = TempProject::with_fixtures(&[]);
  let path = proj.path("dirty.capnp");
  let dirty = "@0xeaf06436acd04fd4;\nstruct A {\n        foo @0:Text;\n}\n";
  std::fs::write(&path, dirty).unwrap();
  let uri = format!("file://{}", path.display());
  c.open(&uri, dirty);

  let r = c.request(
    "textDocument/formatting",
    json!({
        "textDocument": { "uri": uri },
        "options": { "tabSize": 2, "insertSpaces": true },
    }),
  );
  let edits = r["result"].as_array().expect("array of edits");
  assert!(!edits.is_empty(), "expected at least one edit");
  let combined: String = edits
    .iter()
    .map(|e| e["newText"].as_str().unwrap_or(""))
    .collect();
  assert!(
    combined.contains("  foo @0 :Text;"),
    "got combined:\n{combined}"
  );
  c.shutdown();
}

#[test]
fn formatting_returns_empty_for_clean_file() {
  let mut c = LspClient::start();
  let proj = TempProject::with_fixtures(&["types.capnp"]);
  let uri = proj.uri("types.capnp");
  let text = proj.text("types.capnp");
  c.open(&uri, &text);

  // First normalise via our own formatter so the assertion is stable regardless of
  // how the fixture was authored.
  let pre = c.request(
    "textDocument/formatting",
    json!({
        "textDocument": { "uri": uri },
        "options": { "tabSize": 2, "insertSpaces": true },
    }),
  );
  if let Some(edits) = pre["result"].as_array() {
    if let Some(first) = edits.first() {
      let formatted = first["newText"].as_str().unwrap().to_string();
      std::fs::write(proj.path("types.capnp"), &formatted).unwrap();
      c.change(&uri, 2, &formatted);
    }
  }

  let r = c.request(
    "textDocument/formatting",
    json!({
        "textDocument": { "uri": uri },
        "options": { "tabSize": 2, "insertSpaces": true },
    }),
  );
  let edits = r["result"].as_array().expect("array of edits");
  assert!(
    edits.is_empty(),
    "expected no edits on clean file, got {edits:?}"
  );
  c.shutdown();
}

#[test]
fn formatting_skipped_on_parse_error() {
  let mut c = LspClient::start();
  let proj = TempProject::with_fixtures(&[]);
  let path = proj.path("broken.capnp");
  let broken = "@0xeaf06436acd04fd5;\nstruct A { BROKEN_TOKEN!!! }\n";
  std::fs::write(&path, broken).unwrap();
  let uri = format!("file://{}", path.display());
  c.open(&uri, broken);

  let r = c.request(
    "textDocument/formatting",
    json!({
        "textDocument": { "uri": uri },
        "options": { "tabSize": 2, "insertSpaces": true },
    }),
  );
  let edits = r["result"].as_array().expect("array");
  assert!(
    edits.is_empty(),
    "expected no edits on broken file, got {edits:?}"
  );
  c.shutdown();
}

#[test]
fn semantic_tokens_returns_data() {
  let mut c = LspClient::start();
  let proj = user_project();
  let uri = proj.uri("user.capnp");
  let text = proj.text("user.capnp");
  c.open(&uri, &text);

  let r = c.request(
    "textDocument/semanticTokens/full",
    json!({ "textDocument": { "uri": uri } }),
  );
  let data = r["result"]["data"].as_array().expect("data array");
  assert!(!data.is_empty(), "expected semantic tokens");
  assert_eq!(data.len() % 5, 0, "encoded as 5-tuples");
  c.shutdown();
}

#[test]
fn cached_index_survives_compile_failure() {
  let mut c = LspClient::start();
  let proj = user_project();
  let uri = proj.uri("user.capnp");
  let text = proj.text("user.capnp");
  c.open(&uri, &text);

  // Break the file with a syntax error so the next compile fails.
  let broken = format!("{}\nGARBAGE_TOKEN!!!\n", text);
  c.change(&uri, 2, &broken);

  // Completion in a type slot should still see user types from the cached index.
  let (line, col) = locate(&broken, "organisationId @0 :Types.UUID", ":");
  let r = c.request(
    "textDocument/completion",
    json!({ "textDocument": { "uri": uri }, "position": pos(line, col) }),
  );
  let items = r["result"].as_array().expect("items");
  let labels: Vec<&str> =
    items.iter().map(|i| i["label"].as_str().unwrap()).collect();
  assert!(labels.contains(&"AuthToken"), "lost user types: {labels:?}");
  c.shutdown();
}

#[test]
fn live_buffer_changes_visible_to_hover() {
  let mut c = LspClient::start();
  let proj = user_project();
  let uri = proj.uri("user.capnp");
  let text = proj.text("user.capnp");
  c.open(&uri, &text);

  let new_text = text.replace(
    "# Opaque session token used in subsequent requests.",
    "# Opaque session token used in subsequent requests.\n  # ADDED LIVE",
  );
  c.change(&uri, 2, &new_text);

  let (line, col) = locate_inside(&new_text, "List(AuthToken)", "AuthToken");
  let r = c.request(
    "textDocument/hover",
    json!({ "textDocument": { "uri": uri }, "position": pos(line, col) }),
  );
  let value = r["result"]["contents"]["value"].as_str().unwrap();
  assert!(
    value.contains("ADDED LIVE"),
    "live edit not reflected: {value}"
  );
  c.shutdown();
}

/// Apply LSP text edits (sorted by range) to a document string, producing the formatted result.
fn apply_edits(text: &str, edits: &[Value]) -> String {
  let lines: Vec<&str> = text.lines().collect();
  let mut result = String::new();
  let mut cur_line: usize = 0;

  // LSP edits from the formatter are line-granularity replacements, sorted by range.
  for edit in edits {
    let start_line = edit["range"]["start"]["line"].as_u64().unwrap() as usize;
    let start_char =
      edit["range"]["start"]["character"].as_u64().unwrap() as usize;
    let end_line = edit["range"]["end"]["line"].as_u64().unwrap() as usize;
    let end_char = edit["range"]["end"]["character"].as_u64().unwrap() as usize;
    let new_text = edit["newText"].as_str().unwrap();

    // Copy unchanged lines before this edit.
    while cur_line < start_line {
      result.push_str(lines[cur_line]);
      result.push('\n');
      cur_line += 1;
    }

    // Partial start line (chars before start_char).
    if cur_line < lines.len() {
      let line = lines[cur_line];
      let byte_start = line
        .char_indices()
        .nth(start_char)
        .map(|(i, _)| i)
        .unwrap_or(line.len());
      result.push_str(&line[..byte_start]);
    }

    // Insert replacement text.
    result.push_str(new_text);

    // Skip over replaced lines.
    if end_line < lines.len() {
      let end_line_text = lines[end_line];
      let byte_end = end_line_text
        .char_indices()
        .nth(end_char)
        .map(|(i, _)| i)
        .unwrap_or(end_line_text.len());
      result.push_str(&end_line_text[byte_end..]);
      result.push('\n');
      cur_line = end_line + 1;
    } else {
      cur_line = lines.len();
    }
  }

  // Copy remaining lines.
  while cur_line < lines.len() {
    result.push_str(lines[cur_line]);
    result.push('\n');
    cur_line += 1;
  }
  result
}

/// Upstream capnproto schema files copied into tests/fixtures/upstream-*.capnp.
/// These cover annotations, generics, interfaces, enums, and large nested schemas.
const UPSTREAM_FIXTURES: &[&str] = &[
  "upstream-c++.capnp",
  "upstream-persistent.capnp",
  "upstream-schema.capnp",
  "upstream-stream.capnp",
  "upstream-addressbook.capnp",
];

#[test]
fn formatting_upstream_schemas_produces_valid_output() {
  let proj = TempProject::with_fixtures(UPSTREAM_FIXTURES);
  let mut c = LspClient::start();
  let mut failures: Vec<String> = Vec::new();

  for &name in UPSTREAM_FIXTURES {
    let text = proj.text(name);
    let path = proj.path(name);
    let uri = proj.uri(name);
    c.open(&uri, &text);

    let r = c.request(
      "textDocument/formatting",
      json!({
          "textDocument": { "uri": uri },
          "options": { "tabSize": 2, "insertSpaces": true },
      }),
    );

    let formatted = if let Some(edits) = r["result"].as_array() {
      if edits.is_empty() {
        text.clone()
      } else {
        apply_edits(&text, edits)
      }
    } else {
      failures.push(format!("{name}: formatter returned null (parse error)"));
      continue;
    };

    if !validate_capnp(&formatted, &path) {
      failures.push(format!("{name}: formatted output fails capnp compile",));
    }

    // Idempotency: formatting again should produce no edits.
    c.change(&uri, 2, &formatted);
    let r2 = c.request(
      "textDocument/formatting",
      json!({
          "textDocument": { "uri": uri },
          "options": { "tabSize": 2, "insertSpaces": true },
      }),
    );
    if let Some(edits2) = r2["result"].as_array() {
      if !edits2.is_empty() {
        let re_formatted = apply_edits(&formatted, edits2);
        let diff: String = formatted
          .lines()
          .zip(re_formatted.lines())
          .enumerate()
          .filter(|(_, (a, b))| a != b)
          .map(|(i, (a, b))| {
            format!("  line {i}:\n    pass1: {a:?}\n    pass2: {b:?}")
          })
          .take(10)
          .collect::<Vec<_>>()
          .join("\n");
        failures.push(format!("{name}: formatting is not idempotent\n{diff}"));
      }
    }
  }

  c.shutdown();

  if !failures.is_empty() {
    panic!(
      "{} formatting failure(s):\n\n{}",
      failures.len(),
      failures.join("\n\n---\n\n")
    );
  }
}

/// Write text to a temp file alongside the original and run `capnp compile -o-` to validate it.
fn validate_capnp(text: &str, original_path: &std::path::Path) -> bool {
  let dir = original_path.parent().unwrap();
  let tmp = dir.join("__capnprotols_format_check__.capnp");
  std::fs::write(&tmp, text).expect("write temp file");

  let output = Command::new("capnp")
    .arg("compile")
    .arg("-o-")
    .arg(&tmp)
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::piped())
    .output()
    .expect("failed to run capnp compile");

  let _ = std::fs::remove_file(&tmp);

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!(
      "  capnp compile failed for {}:\n{}",
      original_path.display(),
      stderr
    );
  }
  output.status.success()
}
