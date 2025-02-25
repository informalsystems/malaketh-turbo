use clap::Parser;
use color_eyre::eyre;
use tracing::info;

use malachitebft_app::Node;
use malachitebft_config::MetricsConfig;

use crate::metrics;

#[derive(Parser, Debug, Clone, Default, PartialEq)]
pub struct StartCmd {
    #[clap(long)]
    pub start_height: Option<u64>,

    /// Enable the Ethereum JSON-RPC server
    #[clap(long)]
    pub enable_rpc: bool,
}

impl StartCmd {
    pub async fn run(&self, node: impl Node, metrics: Option<MetricsConfig>) -> eyre::Result<()> {
        info!("Node is starting...");

        if self.enable_rpc {
            info!("Starting with RPC server enabled");
        }

        start(node, metrics, self.enable_rpc).await?;

        info!("Node has stopped");

        Ok(())
    }
}

/// start command to run a node.
pub async fn start(node: impl Node, metrics: Option<MetricsConfig>, enable_rpc: bool) -> eyre::Result<()> {
    // Enable Prometheus
    if let Some(metrics) = metrics {
        if metrics.enabled {
            tokio::spawn(metrics::serve(metrics.listen_addr));
        }
    }

    // Start the node
    node.run().await?;

    Ok(())
}
