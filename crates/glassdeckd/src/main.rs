//! glassdeckd: the one program that talks to the keyboard. It owns the Link, runs sources (each in a
//! worker process) and the widgets' PC halves, and takes requests from the CLI over a local API.

mod api;
mod config;
mod deploy;
mod link;
mod pc_host;
mod runner;
mod system;
mod worker;

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::{Arc, mpsc};
use std::thread;

use anyhow::Result;
use clap::{Parser, Subcommand};

use api::Api;
use config::Paths;
use link::Uplink;
use runner::{Runner, RunnerEvent};

#[derive(Parser)]
#[command(about = "Glassdeck's PC host: owns the Link to the keyboard, runs sources and widgets")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run glassdeckd
    Run {
        /// The keyboard's address, overriding config.json
        #[arg(long)]
        keyboard: Option<SocketAddr>,
        /// Write output to the log file and close this console, as when started at login
        #[arg(long)]
        log: bool,
    },
    /// Start glassdeckd at login for this user
    Install,
    /// Stop starting glassdeckd at login
    Uninstall,
    /// A source worker, started by glassdeckd itself
    #[command(hide = true)]
    Source {
        #[arg(long)]
        uplink: SocketAddr,
        #[arg(long)]
        token: String,
    },
}

fn main() -> ExitCode {
    if let Some(code) = glassdeck_capture::helper_process_exit_code() {
        std::process::exit(code);
    }
    let result = match Cli::parse().command {
        Command::Run { keyboard, log } => run(keyboard, log),
        Command::Install => system::install(),
        Command::Uninstall => system::uninstall(),
        Command::Source { uplink, token } => worker::run(uplink, &token),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(keyboard: Option<SocketAddr>, log: bool) -> Result<()> {
    let paths = Paths::for_user()?;
    let _instance = system::single_instance(&paths.api)?;
    if log {
        system::log_to(&paths.log)?;
    }
    let config = config::load(&paths, keyboard)?;
    println!("glassdeckd: keyboard at {}, profile {}", config.keyboard, paths.profile.display());

    let token = new_token()?;
    let (events, inbox) = mpsc::channel();
    let link_up = events.clone();
    let uplink = Arc::new(Uplink::new(move || {
        let _ = link_up.send(RunnerEvent::LinkUp);
    }));
    let runner = Runner::start(Arc::clone(&uplink), token.clone(), events, inbox)?;
    let linked = Arc::clone(&uplink);
    let (to, rate) = (config.keyboard, config.link_bytes_per_sec);
    thread::Builder::new().name("link".into()).spawn(move || link::run(&linked, to, rate))?;
    pc_host::start(paths.profile.clone(), paths.modules.clone(), Arc::clone(&uplink))?;
    let (modules, profile) = (paths.modules.clone(), paths.profile.clone());
    Api { token, runner, uplink, keyboard: config.keyboard, modules, profile }.serve(&paths.api)
}

fn new_token() -> Result<String> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("generating the API token: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
