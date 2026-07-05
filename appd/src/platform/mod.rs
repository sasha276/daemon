#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{install, run_as_service, run_foreground, uninstall};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{install, run_as_service, run_foreground, uninstall};

#[cfg(not(any(windows, target_os = "linux")))]
pub fn run_foreground() {
    generic_run();
}
#[cfg(not(any(windows, target_os = "linux")))]
pub fn run_as_service() {
    generic_run();
}
#[cfg(not(any(windows, target_os = "linux")))]
pub fn install() {
    eprintln!("service install is not supported on this platform");
}
#[cfg(not(any(windows, target_os = "linux")))]
pub fn uninstall() {
    eprintln!("service uninstall is not supported on this platform");
}

#[cfg(not(any(windows, target_os = "linux")))]
fn generic_run() {
    init_tracing();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to create tokio runtime");
    rt.block_on(async {
        let (tx, rx) = tokio::sync::watch::channel(false);
        tokio::select! {
            res = crate::server::run(rx) => {
                if let Err(e) = res {
                    tracing::error!("fatal: {e}");
                    std::process::exit(1);
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("ctrl-c received, shutting down");
                let _ = tx.send(true);
            }
        }
    });
}

pub fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_env("APPD_LOG")
                .add_directive("appd=info".parse().unwrap())
                .add_directive("device_lib=info".parse().unwrap()),
        )
        .init();
}
