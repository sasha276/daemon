mod error;
mod protocol;
mod client;
mod session;
mod device;
mod transport;

use transport::Server;

const BIND_ADDR: &str = "0.0.0.0:1001";

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_env("APPD_LOG")
                .add_directive("appd=info".parse().unwrap())
                .add_directive("device_lib=info".parse().unwrap()),
        )
        .init();

    tracing::info!("appd starting on {BIND_ADDR}");

    if let Err(e) = run().await {
        tracing::error!("fatal: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), error::AppError> {
    let server = Server::bind(BIND_ADDR).await?;
    server.run().await
}
