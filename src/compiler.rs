use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::config::Config;

#[derive(Debug)]
#[allow(dead_code)]
pub struct CompileOutput {
    /// CodeGeneratorRequest bytes from stdout (empty on hard failure).
    pub cgr: Vec<u8>,
    pub stderr: String,
    pub success: bool,
    /// When the compile ran against an overlay file (live unsaved buffer), this is the
    /// overlay's on-disk path. Use it to remap the CGR's compiler-reported paths back to
    /// the real file path before exposing them to LSP clients.
    pub overlay_path: Option<PathBuf>,
}

/// Run `capnp compile -o- <file>` against an on-disk path. We feed the current buffer
/// contents via a temp file rather than stdin because `capnp` resolves imports relative
/// to the file's directory.
pub async fn compile_file(
    config: &Config,
    file_path: &Path,
    overlay_text: Option<&str>,
) -> Result<CompileOutput> {
    let (path_to_compile, _tmp, overlay_path) = match overlay_text {
        Some(text) => {
            let dir = file_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            let name = file_path
                .file_name()
                .map(|s| s.to_owned())
                .unwrap_or_else(|| "buffer.capnp".into());
            // Place the overlay alongside the original so relative imports resolve.
            let mut overlay = dir.clone();
            overlay.push(format!(".capnprotols.{}", name.to_string_lossy()));
            tokio::fs::write(&overlay, text)
                .await
                .with_context(|| format!("writing overlay {}", overlay.display()))?;
            let guard = TempFile(overlay.clone());
            (overlay.clone(), Some(guard), Some(overlay))
        }
        None => (file_path.to_path_buf(), None, None),
    };

    let mut cmd = Command::new(&config.compiler_path);
    cmd.arg("compile").arg("-o-");
    for inc in &config.import_paths {
        cmd.arg("-I").arg(inc);
    }
    cmd.arg(&path_to_compile);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning {}", config.compiler_path))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.shutdown().await;
    }
    let output = child.wait_with_output().await.context("capnp compile wait")?;
    Ok(CompileOutput {
        cgr: output.stdout,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        success: output.status.success(),
        overlay_path,
    })
}

struct TempFile(PathBuf);
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
