use std::ffi::OsString;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState,
        ServiceStatus, ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
};

const SERVICE_NAME: &str = "DaemonAppd";
const SERVICE_DISPLAY: &str = "DaemonAppd USB/CAN Service";

define_windows_service!(ffi_service_main, service_main);

pub fn run_as_service() {
    match service_dispatcher::start(SERVICE_NAME, ffi_service_main) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("Not running as a Windows Service: {e}");
            eprintln!("Use 'appd run' for foreground mode or 'appd install' to register the service.");
            std::process::exit(1);
        }
    }
}

pub fn run_foreground() {
    super::init_tracing();
    tracing::info!("appd running in foreground mode");
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

fn service_main(_args: Vec<OsString>) {
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let shutdown_tx: Arc<Mutex<Option<tokio::sync::watch::Sender<bool>>>> =
        Arc::new(Mutex::new(Some(shutdown_tx)));
    let tx_clone = shutdown_tx.clone();

    let status_handle = service_control_handler::register(SERVICE_NAME, move |control| {
        match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                if let Some(tx) = tx_clone.lock().unwrap().take() {
                    let _ = tx.send(true);
                }
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    })
    .expect("failed to register service control handler");

    let _ = status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    });

    super::init_tracing();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to create tokio runtime");
    let result = rt.block_on(crate::server::run(shutdown_rx));

    let exit_code = if result.is_ok() {
        ServiceExitCode::Win32(0)
    } else {
        ServiceExitCode::ServiceSpecific(1)
    };

    let _ = status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code,
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    });
}

pub fn install() {
    let exe = std::env::current_exe().expect("failed to get exe path");
    let bin_path = format!("\"{}\" run", exe.display());

    let output = Command::new("sc")
        .arg("create")
        .arg(SERVICE_NAME)
        .arg("binPath=")
        .arg(&bin_path)
        .arg("start=")
        .arg("auto")
        .arg("DisplayName=")
        .arg(SERVICE_DISPLAY)
        .output()
        .expect("failed to run sc.exe");

    if output.status.success() {
        println!("Service '{SERVICE_NAME}' installed successfully.");
        println!("Start it with: sc start {SERVICE_NAME}");
        println!("Or via Services (services.msc).");
    } else {
        eprintln!("Failed to install service:");
        eprintln!("{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        std::process::exit(1);
    }
}

pub fn uninstall() {
    let _ = Command::new("sc")
        .args(["stop", SERVICE_NAME])
        .output();

    let output = Command::new("sc")
        .args(["delete", SERVICE_NAME])
        .output()
        .expect("failed to run sc.exe");

    if output.status.success() {
        println!("Service '{SERVICE_NAME}' uninstalled.");
    } else {
        eprintln!("Failed to uninstall service:");
        eprintln!("{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        std::process::exit(1);
    }
}
