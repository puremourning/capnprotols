use std::sync::Arc;

use dashmap::DashMap;
use ropey::Rope;
use tower_lsp::lsp_types::{Position, Range, TextDocumentContentChangeEvent, Url};

#[derive(Debug, Clone)]
pub struct Document {
    pub version: i32,
    pub rope: Rope,
}

impl Document {
    pub fn new(text: String, version: i32) -> Self {
        Self {
            version,
            rope: Rope::from_str(&text),
        }
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    /// Apply an LSP incremental change. If `range` is None, replace the whole buffer.
    pub fn apply_change(&mut self, change: TextDocumentContentChangeEvent) {
        match change.range {
            None => {
                self.rope = Rope::from_str(&change.text);
            }
            Some(range) => {
                let start = position_to_char(&self.rope, range.start);
                let end = position_to_char(&self.rope, range.end);
                self.rope.remove(start..end);
                self.rope.insert(start, &change.text);
            }
        }
    }
}

fn position_to_char(rope: &Rope, pos: Position) -> usize {
    let line = (pos.line as usize).min(rope.len_lines().saturating_sub(1));
    let line_start = rope.line_to_char(line);
    let line_slice = rope.line(line);
    // LSP uses UTF-16 code units; for ASCII schemas this matches chars. Approximate with chars
    // for now; fix to UTF-16 when we hit non-ASCII identifiers.
    let col = (pos.character as usize).min(line_slice.len_chars());
    line_start + col
}

#[allow(dead_code)]
pub fn range_to_chars(rope: &Rope, range: Range) -> std::ops::Range<usize> {
    position_to_char(rope, range.start)..position_to_char(rope, range.end)
}

#[derive(Debug, Default, Clone)]
pub struct DocumentStore {
    inner: Arc<DashMap<Url, Document>>,
}

impl DocumentStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open(&self, uri: Url, text: String, version: i32) {
        self.inner.insert(uri, Document::new(text, version));
    }

    pub fn close(&self, uri: &Url) {
        self.inner.remove(uri);
    }

    pub fn update(
        &self,
        uri: &Url,
        version: i32,
        changes: Vec<TextDocumentContentChangeEvent>,
    ) -> Option<String> {
        let mut entry = self.inner.get_mut(uri)?;
        for change in changes {
            entry.apply_change(change);
        }
        entry.version = version;
        Some(entry.text())
    }

    pub fn get_text(&self, uri: &Url) -> Option<String> {
        self.inner.get(uri).map(|d| d.text())
    }
}
