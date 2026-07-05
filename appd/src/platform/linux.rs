use std::process::Command;

const SERVICE_NAME: &str = "daemonappd";
const SERVICE_FILE: &str = "/etc/systemd/system/daemonappd.service";

pub fn run_foreground() {
    super::init_tracing();
    tracing::info!("appd running in foreground");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to create tokio runtime");
    rt.block_on(run_with_signals());
}

pub fn run_as_service() {
    super::init_tracing();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to create tokio runtime");
    rt.block_on(run_with_signals());
}

async fn run_with_signals() {
    use tokio::signal::unix::{signal, SignalKind};

    let (tx, rx) = tokio::sync::watch::channel(false);

    let mut sigterm = signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
    let mut sigint  = signal(SignalKind::interrupt()).expect("failed to register SIGINT handler");

    tokio::select! {
        res = crate::server::run(rx) => {
            if let Err(e) = res {
                tracing::error!("fatal: {e}");
                std::process::exit(1);
            }
        }
        _ = sigterm.recv() => {
            tracing::info!("SIGTERM received, shutting down");
            let _ = tx.send(true);
        }
        _ = sigint.recv() => {
            tracing::info!("SIGINT received, shutting down");
            let _ = tx.send(true);
        }
    }
}

pub fn install() {
    let exe = std::env::current_exe().expect("failed to get exe path");
    let exe_str = exe.to_string_lossy();

    let unit = format!(
        "[Unit]\n\
         Description=DaemonAppd USB/CAN service\n\
         After=network.target\n\
         \n\
         [Service]\n\
         ExecStart={exe_str} run\n\
         Restart=on-failure\n\
         User=root\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n"
    );

    std::fs::write(SERVICE_FILE, unit).unwrap_or_else(|e| {
        eprintln!("failed to write {SERVICE_FILE}: {e}");
        eprintln!("try running with sudo");
        std::process::exit(1);
    });

    let status = Command::new("systemctl")
        .args(["enable", SERVICE_NAME])
        .status()
        .expect("failed to run systemctl");

    if status.success() {
        println!("Service '{SERVICE_NAME}' installed and enabled.");
        println!("Start it now with: systemctl start {SERVICE_NAME}");
    } else {
        eprintln!("systemctl enable failed");
        std::process::exit(1);
    }
}

pub fn uninstall() {
    let _ = Command::new("systemctl")
        .args(["stop", SERVICE_NAME])
        .status();

    let status = Command::new("systemctl")
        .args(["disable", SERVICE_NAME])
        .status()
        .expect("failed to run systemctl");

    if !status.success() {
        eprintln!("systemctl disable failed (service may not be installed)");
    }

    if let Err(e) = std::fs::remove_file(SERVICE_FILE) {
        eprintln!("failed to remove {SERVICE_FILE}: {e}");
    }

    println!("Service '{SERVICE_NAME}' uninstalled.");
}
