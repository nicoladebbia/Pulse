mod pipeline;
mod sources;
mod claude;
mod dedup;
mod embeddings;

use clap::Parser;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "pulse-fetcher", about = "Daily news fetch pipeline for Pulse")]
struct Args {
    /// Fetch mode: 'daily' for scheduled fetch, 'manual' for on-demand
    #[arg(long, default_value = "daily")]
    mode: String,

    /// Path to the SQLite database
    #[arg(long)]
    db_path: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("pulse_fetcher=info".parse()?))
        .init();

    let args = Args::parse();

    let db_path = args.db_path.unwrap_or_else(|| {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("com.pulse.app")
            .join("pulse.db")
    });

    tracing::info!("Pulse fetcher starting in {} mode", args.mode);
    tracing::info!("Database: {}", db_path.display());

    pipeline::run(&db_path).await?;

    tracing::info!("Pulse fetch complete");
    Ok(())
}
