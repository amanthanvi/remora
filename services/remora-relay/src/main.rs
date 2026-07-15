use std::{path::PathBuf, sync::Arc};

use clap::{Parser, Subcommand};
use remora_relay::{
    ApiState, PushDispatcher, RelayConfig, RelayMetrics, build_router, worker::unix_time_ms,
};
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "remora-relay", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate configuration without opening the database or provider network.
    CheckConfig {
        #[arg(long)]
        config: PathBuf,
    },
    /// Run the API, outbox dispatcher, and retention maintenance loops.
    Serve {
        #[arg(long)]
        config: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .json()
        .with_current_span(false)
        .with_span_list(false)
        .init();

    match Cli::parse().command {
        Command::CheckConfig { config } => {
            RelayConfig::load(config)?;
            println!("configuration valid");
        }
        Command::Serve { config } => serve(RelayConfig::load(config)?).await?,
    }
    Ok(())
}

async fn serve(config: RelayConfig) -> remora_relay::Result<()> {
    let metrics = Arc::new(RelayMetrics::default());
    let backend = config.build_backend(metrics.clone()).await?;
    let providers = config.build_providers()?;
    let bootstrap_auth = config.bootstrap_auth()?;
    let dispatcher = PushDispatcher::new(backend.clone(), providers, config.worker.batch_size)?;
    let app = build_router(ApiState {
        backend: backend.clone(),
        metrics,
        bootstrap_auth,
        max_body_bytes: config.limits.max_http_body_bytes(),
    });
    let listener = tokio::net::TcpListener::bind(config.server.bind).await?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let dispatch = tokio::spawn({
        let dispatcher = dispatcher.clone();
        let shutdown = shutdown_rx.clone();
        let poll_interval = config.poll_interval();
        async move { dispatcher.run(poll_interval, shutdown).await }
    });
    let maintenance = tokio::spawn({
        let backend = backend.clone();
        let mut shutdown = shutdown_rx.clone();
        let maintenance_interval = config.maintenance_interval();
        async move {
            let mut interval = tokio::time::interval(maintenance_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if backend.maintenance(unix_time_ms()).await.is_err() {
                            tracing::warn!(error_kind = "storage", "relay maintenance failed");
                        }
                    }
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow() {
                            break;
                        }
                    }
                }
            }
        }
    });

    tracing::info!(profile = ?config.deployment_profile, "Remora relay ready");
    let server = axum::serve(listener, app).with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    });
    let result = server.await.map_err(remora_relay::RelayError::Io);
    let _ = shutdown_tx.send(true);
    let _ = dispatch.await;
    let _ = maintenance.await;
    result
}
