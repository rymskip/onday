mod config;
mod params;
mod server;
mod session;

use std::sync::Mutex;

use anyhow::Context;
use clap::Parser;
use rmcp::ServiceExt;
use tokio::signal::unix::{SignalKind, signal};
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::session::SessionDir;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();
    let dir = SessionDir::open(&config.output_dir, config.session.clone())?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.mcp_log())
        .with_context(|| format!("open {}", dir.mcp_log().display()))?;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(Mutex::new(log))
        .with_ansi(false)
        .init();
    tracing::info!("session {} at {}", dir.id, dir.root.display());

    let server = server::OndayServer::new(config, dir);
    let service = server
        .clone()
        .serve(rmcp::transport::io::stdio())
        .await
        .context("start the MCP server")?;
    let mut terminate = signal(SignalKind::terminate()).context("listen for SIGTERM")?;
    let mut interrupt = signal(SignalKind::interrupt()).context("listen for SIGINT")?;
    tokio::select! {
        finished = service.waiting() => {
            finished.context("MCP transport")?;
        }
        _ = terminate.recv() => tracing::info!("SIGTERM; shutting down"),
        _ = interrupt.recv() => tracing::info!("SIGINT; shutting down"),
    }
    server.shutdown().await;
    Ok(())
}
