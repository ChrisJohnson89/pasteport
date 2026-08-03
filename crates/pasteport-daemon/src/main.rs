//! `pasteportd` — the Pasteport background service.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use clap::Parser;

use pasteport_core::{paths, Config, Store};
use pasteport_daemon::{server, service::Service, spawn_watcher};

#[derive(Debug, Parser)]
#[command(
    name = "pasteportd",
    version,
    about = "Pasteport clipboard history service",
    long_about = "Watches the system clipboard, stores history locally, and serves \
                  a Unix-socket API to the Pasteport apps and CLI."
)]
struct Args {
    /// Override the data directory (database, config, socket).
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// Override the control socket path.
    #[arg(long, value_name = "PATH")]
    socket: Option<PathBuf>,

    /// Poll interval in milliseconds. Overrides the config file.
    #[arg(long, value_name = "MS")]
    poll_interval: Option<u64>,

    /// Keep history in memory only: nothing is written to disk.
    #[arg(long)]
    private: bool,

    /// Log level: error, warn, info, debug, trace.
    #[arg(long, default_value = "info", value_name = "LEVEL")]
    log: String,

    /// Capture whatever is already on the clipboard at startup.
    #[arg(long)]
    capture_on_start: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    init_tracing(&args.log);

    // --data-dir works by setting the variable the path helpers read, so every
    // path stays derived from one place.
    if let Some(dir) = &args.data_dir {
        std::env::set_var("PASTEPORT_DATA_DIR", dir);
    }

    let data_dir = paths::ensure_data_dir().context("could not create the data directory")?;
    let socket = match &args.socket {
        Some(p) => p.clone(),
        None => paths::socket_path()?,
    };

    let had_config = Config::exists()?;
    let mut config = Config::load().context("could not load config")?;
    if !had_config {
        // Write the defaults out on first run so there is something to edit.
        // Done before the CLI override is applied, so `--poll-interval` stays a
        // one-off rather than being silently persisted.
        let path = paths::config_path()?;
        if let Err(e) = config.save_to(&path) {
            tracing::warn!(error = %e, "could not write the default config");
        } else {
            tracing::info!(path = %path.display(), "wrote default config");
        }
    }
    if let Some(ms) = args.poll_interval {
        config.poll_interval_ms = ms.clamp(50, 60_000);
    }

    let store = if args.private {
        tracing::warn!("private mode: history is in memory and will be lost on exit");
        Arc::new(std::sync::Mutex::new(Store::open_in_memory()?))
    } else {
        pasteport_daemon::open_store(&paths::database_path()?)?
    };

    // Two independent clipboard handles: one for the watcher, one for writes.
    let watch_backend = pasteport_clipboard::default_backend()
        .context("no clipboard backend available on this system")?;
    let write_backend = pasteport_clipboard::default_backend()
        .context("no clipboard backend available on this system")?;
    tracing::info!(backend = watch_backend.name(), "clipboard backend");

    let service = Arc::new(Service::new(store, config, write_backend, data_dir.clone()));

    if args.capture_on_start {
        match service.handle(pasteport_daemon::Request::CaptureNow) {
            resp if resp.is_error() => tracing::debug!(?resp, "nothing to capture at startup"),
            _ => tracing::info!("captured the clipboard contents present at startup"),
        }
    }

    let listener = server::bind(&socket)?;
    server::install_signal_handlers(&service, &socket);
    let watcher = spawn_watcher(Arc::clone(&service), watch_backend)?;

    println!(
        "pasteportd {} listening on {}",
        pasteport_core::VERSION,
        socket.display()
    );
    let result = server::serve(Arc::clone(&service), listener, &socket);

    // Let the watcher thread notice the shutdown flag and finish its poll.
    if let Err(e) = watcher.join() {
        tracing::warn!("watcher thread panicked: {e:?}");
    }
    server::cleanup(&socket);
    result
}

fn init_tracing(level: &str) {
    use tracing_subscriber::{fmt, EnvFilter};
    // RUST_LOG wins when set, so `--log` is a convenience rather than a cage.
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("pasteport={level},pasteportd={level}")));
    fmt().with_env_filter(filter).with_target(true).init();
}
