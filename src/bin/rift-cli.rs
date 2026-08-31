//! The client binary.
//!
//! `rift` itself now accepts every subcommand here, so this exists for the
//! scripts, hotkeys and Homebrew installs that already call `rift-cli`. It is a
//! second entry point into the same code, not a second implementation: the
//! command tree lives in [`rift_wm::cli`].

use std::process;

use clap::{Parser, Subcommand};
use rift_wm::cli::{self, ClientCommand};
use rift_wm::sys::service::{ServiceCommands, handle_service_command};

#[derive(Parser)]
#[command(name = "rift-cli")]
#[command(about = "Command-line interface for rift window manager")]
#[command(
    long_about = "Command-line interface for the rift window manager.\n\nThe `rift` binary accepts these same subcommands (`rift query windows`); \
`rift-cli` is kept so existing scripts keep working."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage the launchd service for rift
    Service {
        #[command(subcommand)]
        service: ServiceCommands,
    },
    #[command(flatten)]
    Client(ClientCommand),
}

fn main() {
    sigpipe::reset();

    match Cli::parse().command {
        Commands::Service { service } => match handle_service_command(&service) {
            Ok(message) => println!("{message}"),
            Err(error) => {
                eprintln!("rift-cli: {error}");
                process::exit(1);
            }
        },
        Commands::Client(command) => cli::run(command),
    }
}
