mod error;
mod protocol;
mod client;
mod session;
mod device;
mod transport;
mod server;
mod platform;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "appd", about = "DaemonAppd USB/CAN service")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run in foreground (for debugging / manual start)
    Run,
    /// Register as an autostart system service
    Install,
    /// Remove system service registration
    Uninstall,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        None                      => platform::run_as_service(),
        Some(Commands::Run)       => platform::run_foreground(),
        Some(Commands::Install)   => platform::install(),
        Some(Commands::Uninstall) => platform::uninstall(),
    }
}
