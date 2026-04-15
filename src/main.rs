use tower_lsp::{LspService, Server};

mod aliases;
mod compiler;
mod diagnostics;
mod document;
mod index;
mod ordinals;
mod schema_capnp;
mod semantic_tokens;
mod server;

// Re-export from lib crate so binary-local modules can use `crate::config` / `crate::format`.
pub use capnprotols::config;
pub use capnprotols::format;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
  tracing_subscriber::fmt()
    .with_writer(std::io::stderr)
    .with_ansi(false)
    .with_env_filter(
      tracing_subscriber::EnvFilter::try_from_env("CAPNPROTOLS_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
    )
    .init();

  let stdin = tokio::io::stdin();
  let stdout = tokio::io::stdout();

  let (service, socket) = LspService::new(server::Backend::new);
  Server::new(stdin, stdout, socket).serve(service).await;
}
