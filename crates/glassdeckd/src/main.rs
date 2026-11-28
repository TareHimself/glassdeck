//! glassdeckd: the one program that talks to the keyboard. It owns the Link, runs sources (each in a
//! worker process) and the widgets' PC halves, sends streams to WebRTC viewers, and takes requests
//! from the CLI over a local API.

mod active;
mod api;
mod config;
mod error;
mod feeds;
mod link;
mod module_host;
mod runner;
mod streams;
mod system;
mod theme;
mod usb_link;
mod whep;
mod worker;

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use anyhow::Result;
use clap::{Parser, Subcommand};

use active::ActivePackages;
use api::Api;
use config::{KeyboardAddress, Paths};
use link::Uplink;
use glassdeck_api::{Event, KeyboardLink};
use runner::{Runner, RunnerEvent, StatusBoard};
use tokio::sync::broadcast;

#[derive(Parser)]
#[command(about = "Glassdeck's host: owns the Link to the keyboard, runs sources and widgets' host halves")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run glassdeckd
    Run {
        /// `usb`, or the keyboard's network address, overriding config.json
        #[arg(long)]
        keyboard: Option<KeyboardAddress>,
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

fn run(keyboard: Option<KeyboardAddress>, log: bool) -> Result<()> {
    let paths = Paths::for_user()?;
    let _instance = system::single_instance(&paths.api)?;
    if log {
        system::log_to(&paths.log)?;
    }
    let config = config::load(&paths, keyboard)?;
    println!("glassdeckd: keyboard at {}, theme {}", config.keyboard, paths.theme.display());

    let token = new_token()?;
    let (published, _) = broadcast::channel(api::EVENT_BACKLOG);
    let (events, inbox) = mpsc::channel();
    let (link_up, link_changed) = (events.clone(), published.clone());
    let (reported, keyboard_reports) = mpsc::channel::<()>();
    let (module_host, module_host_inbox) = mpsc::channel();
    let heard = module_host.clone();
    let keyboard_news = module_host.clone();
    let uplink = Arc::new(Uplink::new(
        move |connection| {
            if connection == KeyboardLink::Connected {
                let _ = link_up.send(RunnerEvent::LinkUp);
            }
            let _ = keyboard_news.send(module_host::HostCommand::Keyboard(module_host::keyboard_is_there(&connection)));
            let _ = link_changed.send(Event::Link { connection });
        },
        move || {
            let _ = reported.send(());
        },
        move |from, sent| {
            let _ = heard.send(module_host::HostCommand::Message(from, sent));
        },
    ));
    let stream_changed = published.clone();
    let status = Arc::new(StatusBoard::new(move |status| {
        let _ = stream_changed.send(Event::Stream { status: status.clone() });
    }));
    let feeds = feeds::Feeds::new(token.clone());
    let runner = Runner::start(Arc::clone(&uplink), token.clone(), events, inbox, status, Arc::clone(&feeds))?;
    let linked = Arc::clone(&uplink);
    let to = match config.keyboard {
        KeyboardAddress::Network(address) => address,
        KeyboardAddress::Usb => usb_link::start()?,
    };
    let rate = config.link_bytes_per_sec;
    thread::Builder::new().name("link".into()).spawn(move || link::run(&linked, to, rate))?;
    let held = ActivePackages::open(paths.active.clone())?;
    let (kept, active) = restored_theme(&paths)?;
    module_host::start(paths.active.clone(), active.clone(), Arc::clone(&uplink), glassdeck_host::start_media()?, module_host.clone(), module_host_inbox)?;
    let api = Arc::new(Api {
        token,
        runner,
        uplink,
        keyboard: config.keyboard,
        active: Mutex::new(held),
        large_uploads: Mutex::new(0),
        theme_file: paths.theme.clone(),
        theme: Mutex::new((kept, active)),
        module_host,
        events: published,
        feeds,
        whep: Arc::default(),
    });
    if let Err(e) = api.restore() {
        eprintln!("error: restoring the theme: {e:#}; the keyboard keeps its own");
    }
    let syncing = Arc::clone(&api);
    thread::Builder::new().name("keyboard sync".into()).spawn(move || {
        for () in keyboard_reports {
            syncing.keyboard_reported();
        }
    })?;
    api.serve(&paths.api)
}

/// The theme kept from last time, or the starting one if it no longer checks out against what's installed.
fn restored_theme(paths: &Paths) -> Result<(glassdeck_api::Theme, theme::ActiveTheme)> {
    let installed = theme::installed(&paths.active)?;
    let kept = theme::load(&paths.theme)?;
    match theme::ActiveTheme::check(&kept, &installed) {
        Ok(active) => Ok((kept, active)),
        Err(e) => {
            eprintln!("error: the kept theme: {e:#}; starting from the built-in gradient");
            let starting = glassdeck_api::Theme::starting(glassdeck_proto::BUILT_IN_BACKGROUND);
            let active = theme::ActiveTheme::check(&starting, &installed)?;
            Ok((starting, active))
        }
    }
}

fn new_token() -> Result<String> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("generating the API token: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
