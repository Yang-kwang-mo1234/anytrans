//! `anytrans`: pair devices and move files between them from a terminal.

use std::{
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use anytrans_core::{Direction, Event, Node, NodeOptions, PathKind};
use clap::{Parser, Subcommand};
use tokio::sync::broadcast::{error::RecvError, Receiver};

#[derive(Parser)]
#[command(
    name = "anytrans",
    version,
    about = "Send files between your own devices"
)]
struct Cli {
    /// Where the device key and paired devices are stored
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    /// Where received files are saved
    #[arg(long, global = true)]
    dir: Option<PathBuf>,
    /// Name other devices see for this one (remembered once set)
    #[arg(long, global = true)]
    name: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Pair with another device: without CODE, show a code and wait;
    /// with CODE, join the device that showed it
    Pair { code: Option<String> },
    /// List paired devices
    Devices,
    /// Remove a paired device
    Forget { device: String },
    /// Send files or folders to a paired device
    Send {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Name or id prefix of the receiving device
        #[arg(long)]
        to: String,
    },
    /// Stay running and save whatever paired devices send
    Receive,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let data_dir = match cli.data_dir {
        Some(dir) => dir,
        None => dirs::config_dir()
            .context("cannot find the user config directory; pass --data-dir")?
            .join("anytrans"),
    };
    let download_dir = match cli.dir {
        Some(dir) => dir,
        None => dirs::download_dir()
            .context("cannot find the Downloads folder; pass --dir")?
            .join("AnyTrans"),
    };
    let node = Node::start(NodeOptions {
        data_dir,
        download_dir,
        device_name: cli.name,
        mdns: true,
        local_only: false,
    })
    .await?;

    let result = run(&node, cli.command).await;
    node.shutdown().await;
    result
}

async fn run(node: &Node, command: Command) -> Result<()> {
    match command {
        Command::Pair { code: Some(code) } => {
            let peer = node.pair(&code).await?;
            println!("Paired with {}", peer.name);
        }
        Command::Pair { code: None } => {
            let mut events = node.subscribe();
            println!(
                "On the other device, run:\n\n  anytrans pair {}\n",
                node.pairing_ticket().await?
            );
            println!("Waiting (the code is valid for 5 minutes, once)...");
            let paired = async {
                loop {
                    if let Some(Event::Paired { peer }) = next_event(&mut events).await {
                        return peer;
                    }
                }
            };
            match tokio::time::timeout(Duration::from_secs(300), paired).await {
                Ok(peer) => println!("Paired with {}", peer.name),
                Err(_) => bail!("the code expired before a device used it"),
            }
        }
        Command::Devices => {
            println!("This device: {} ({})", node.name(), node.id().fmt_short());
            let peers = node.peers();
            if peers.is_empty() {
                println!("No paired devices. Run `anytrans pair` to add one.");
            }
            for peer in peers {
                println!("  {}  {}", peer.id.fmt_short(), peer.name);
            }
        }
        Command::Forget { device } => {
            let peer = node.find_peer(&device)?;
            node.forget_peer(&peer.id)?;
            println!("Removed {}", peer.name);
        }
        Command::Send { paths, to } => {
            let peer = node.find_peer(&to)?;
            let events = node.subscribe();
            let printer = tokio::spawn(print_events(events, true));
            let sent = node.send(&peer.id, paths).await;
            let _ = printer.await;
            sent?;
        }
        Command::Receive => {
            println!(
                "{} is receiving into {} (Ctrl-C to stop)",
                node.name(),
                node.download_dir().display()
            );
            tokio::select! {
                _ = print_events(node.subscribe(), false) => {}
                _ = tokio::signal::ctrl_c() => println!(),
            }
        }
    }
    Ok(())
}

/// The next event, or `None` if some were skipped because we fell behind.
async fn next_event(events: &mut Receiver<Event>) -> Option<Event> {
    match events.recv().await {
        Ok(event) => Some(event),
        Err(RecvError::Lagged(_)) => None,
        Err(RecvError::Closed) => std::future::pending().await,
    }
}

/// Prints transfer activity. With `until_done`, returns after the first
/// transfer ends.
async fn print_events(mut events: Receiver<Event>, until_done: bool) {
    let mut started = Instant::now();
    loop {
        let Some(event) = next_event(&mut events).await else {
            continue;
        };
        match event {
            Event::Paired { peer } => println!("Paired with {}", peer.name),
            Event::TransferStarted {
                peer,
                direction,
                files,
                total_bytes,
                resumed_bytes,
                ..
            } => {
                started = Instant::now();
                let verb = match direction {
                    Direction::Send => "Sending to",
                    Direction::Receive => "Receiving from",
                };
                print!("{verb} {peer}: {files} file(s), {}", human(total_bytes));
                if resumed_bytes > 0 {
                    print!(" (resuming at {})", human(resumed_bytes));
                }
                println!();
            }
            Event::Progress {
                done_bytes,
                total_bytes,
                path,
                ..
            } => {
                let percent = (done_bytes * 100).checked_div(total_bytes).unwrap_or(100);
                let via = match path {
                    PathKind::Lan => "local network",
                    PathKind::Direct => "direct",
                    PathKind::Relay => "relay",
                    PathKind::Unknown => "connecting",
                };
                print!(
                    "\r  {percent:3}%  {} / {}  via {via}        ",
                    human(done_bytes),
                    human(total_bytes)
                );
                let _ = std::io::stdout().flush();
            }
            Event::TransferFinished { saved, .. } => {
                println!("\nDone in {:.1}s", started.elapsed().as_secs_f32());
                for path in saved {
                    println!("  saved {}", path.display());
                }
                if until_done {
                    return;
                }
            }
            Event::TransferFailed { error, .. } => {
                println!("\nFailed: {error}");
                if until_done {
                    return;
                }
            }
        }
    }
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
