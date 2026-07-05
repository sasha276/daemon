use tokio::sync::watch;

use crate::{error::AppError, transport::Server};

const BIND_ADDR: &str = "0.0.0.0:8000";

pub async fn run(mut shutdown: watch::Receiver<bool>) -> Result<(), AppError> {
    tracing::info!("appd starting on {BIND_ADDR}");
    let server = Server::bind(BIND_ADDR).await?;

    let mut handle = tokio::spawn(server.run());

    tokio::select! {
        _ = async {
            loop {
                if *shutdown.borrow() { return; }
                if shutdown.changed().await.is_err() { return; }
            }
        } => {
            tracing::info!("shutdown signal received, stopping server");
            handle.abort();
            Ok(())
        }
        res = &mut handle => {
            match res {
                Ok(r)  => r,
                Err(e) if e.is_cancelled() => Ok(()),
                Err(_) => Ok(()),
            }
        }
    }
}
