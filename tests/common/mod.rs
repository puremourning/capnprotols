//! Shared LSP test client. Spawns the `capnprotols` binary and speaks LSP over stdio,
//! offering synchronous request/notify primitives plus polled response collection.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Convenient handle on a running language server that we can drive from tests.
pub struct LspClient {
    child: Option<Child>,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    next_id: i64,
}

impl LspClient {
    /// Spawn the server binary and complete the LSP `initialize` handshake.
    pub fn start() -> Self {
        let bin = env!("CARGO_BIN_EXE_capnprotols");
        let mut child = Command::new(bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CAPNPROTOLS_LOG", "warn")
            .spawn()
            .expect("failed to spawn capnprotols binary");

        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let (tx, rx) = mpsc::channel::<Value>();
        thread::spawn(move || drain_stdout(stdout, tx));

        // Ditch stderr so the child doesn't block on a full pipe in long tests.
        if let Some(stderr) = child.stderr.take() {
            thread::spawn(move || {
                let mut sink = Vec::new();
                let _ = std::io::copy(&mut std::io::BufReader::new(stderr), &mut sink);
            });
        }

        let mut c = Self {
            child: Some(child),
            stdin,
            rx,
            next_id: 1,
        };
        let init = c.request(
            "initialize",
            json!({
                "capabilities": {},
                "processId": std::process::id(),
                "rootUri": null,
            }),
        );
        assert!(init.get("result").is_some(), "initialize failed: {init}");
        c.notify("initialized", json!({}));
        c
    }

    /// Send didOpen and wait for the first publishDiagnostics. Returns the diagnostics
    /// array so tests can assert on them.
    pub fn open(&mut self, uri: &str, text: &str) -> Vec<Value> {
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "capnp",
                    "version": 1,
                    "text": text,
                }
            }),
        );
        self.next_diagnostics()
    }

    /// Send didChange and wait for the resulting publishDiagnostics.
    pub fn change(&mut self, uri: &str, version: i64, text: &str) -> Vec<Value> {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": text }],
            }),
        );
        self.next_diagnostics()
    }

    /// Wait for the next `textDocument/publishDiagnostics` and return its diagnostics array.
    pub fn next_diagnostics(&mut self) -> Vec<Value> {
        let pd = self
            .wait_notification("textDocument/publishDiagnostics", Duration::from_secs(8))
            .expect("publishDiagnostics");
        pd.pointer("/params/diagnostics")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
    }

    pub fn shutdown(mut self) {
        let _ = self.request_no_params("shutdown");
        self.notify_no_params("exit");
        if let Some(mut child) = self.child.take() {
            // Don't block forever if the child somehow hangs — tests must always finish.
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) => {
                        if Instant::now() >= deadline {
                            let _ = child.kill();
                            let _ = child.wait();
                            return;
                        }
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => return,
                }
            }
        }
    }

    /// Send a parameterless request (some LSP methods reject explicit null params).
    pub fn request_no_params(&mut self, method: &str) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method }));
        self.await_response(id, method)
    }

    pub fn notify_no_params(&mut self, method: &str) {
        self.send(json!({ "jsonrpc": "2.0", "method": method }));
    }

    /// Send a request and synchronously wait for its matching response.
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        self.await_response(id, method)
    }

    fn await_response(&mut self, id: i64, method: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::ZERO);
            let msg = self
                .rx
                .recv_timeout(remaining)
                .unwrap_or_else(|_| panic!("timed out waiting for response to {method}"));
            if msg.get("id").and_then(|v| v.as_i64()) == Some(id) {
                return msg;
            }
        }
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }));
    }

    /// Drain notifications until one matching `method` arrives, or the deadline elapses.
    pub fn wait_notification(&mut self, method: &str, max_wait: Duration) -> Option<Value> {
        let deadline = Instant::now() + max_wait;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::ZERO);
            if remaining.is_zero() {
                return None;
            }
            let msg = self.rx.recv_timeout(remaining).ok()?;
            if msg.get("method").and_then(|v| v.as_str()) == Some(method) {
                return Some(msg);
            }
        }
    }

    fn send(&mut self, msg: Value) {
        let body = msg.to_string();
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.stdin.write_all(header.as_bytes()).expect("stdin");
        self.stdin.write_all(body.as_bytes()).expect("stdin");
        self.stdin.flush().expect("stdin flush");
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn drain_stdout(stdout: ChildStdout, tx: mpsc::Sender<Value>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                break;
            }
            if let Some(rest) = line.strip_prefix("Content-Length: ") {
                content_length = rest.trim().parse().ok();
            }
        }
        let len = content_length.expect("missing Content-Length");
        let mut buf = vec![0u8; len];
        if reader.read_exact(&mut buf).is_err() {
            return;
        }
        let value: Value = match serde_json::from_slice(&buf) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if tx.send(value).is_err() {
            return;
        }
    }
}

/// A throwaway project directory with copies of named fixtures. Each test gets its own
/// so the overlay file capnprotols writes alongside the source doesn't collide with
/// other tests running in parallel.
pub struct TempProject {
    dir: PathBuf,
}

impl TempProject {
    /// Create a fresh temp dir and copy the named fixtures (e.g. `["user.capnp", "types.capnp"]`)
    /// into it. The fixtures live under `tests/fixtures/` in the crate.
    pub fn with_fixtures(names: &[&str]) -> Self {
        let mut src_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        src_root.push("tests/fixtures");

        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "capnprotols-test-{}-{}-{}",
            std::process::id(),
            nanos,
            n
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        for name in names {
            let src = src_root.join(name);
            let dst = dir.join(name);
            std::fs::copy(&src, &dst)
                .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", src.display(), dst.display()));
        }
        Self { dir }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub fn uri(&self, name: &str) -> String {
        format!("file://{}", self.path(name).display())
    }

    pub fn text(&self, name: &str) -> String {
        let p = self.path(name);
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
