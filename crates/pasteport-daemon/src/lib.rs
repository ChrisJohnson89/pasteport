//! The Pasteport background service.
//!
//! Splitting this into a library plus a thin binary means the CLI and the two
//! GUI apps share one definition of the protocol, and the request handling is
//! testable without spawning a process.

#![warn(missing_debug_implementations)]

pub mod protocol;
pub mod server;
pub mod service;

pub use protocol::{Request, Response, StatusReport};
pub use server::{is_running, Client};
pub use service::Service;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pasteport_clipboard::Watcher;
use pasteport_core::{InsertOutcome, Store};

/// Start the clipboard watcher on its own thread.
///
/// The watcher owns its own clipboard handle, separate from the one the service
/// uses for writes, so a slow paste never delays capture and vice versa.
pub fn spawn_watcher(
    service: Arc<Service>,
    backend: Box<dyn pasteport_clipboard::ClipboardBackend>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    let interval = Duration::from_millis(service.config().poll_interval_ms);
    let shutdown = service.shutdown_handle();

    std::thread::Builder::new()
        .name("pasteport-watcher".into())
        .spawn(move || {
            let mut watcher = Watcher::new(backend, interval);
            let stop = watcher.stop_handle();

            // Bridge the service's shutdown flag to the watcher's.
            let bridge = std::thread::Builder::new()
                .name("pasteport-watch-stop".into())
                .spawn(move || {
                    while !shutdown.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(150));
                    }
                    stop.store(true, Ordering::Relaxed);
                })
                .expect("failed to spawn watcher stop bridge");

            watcher.run(|clip| match service.ingest(&clip) {
                Ok(InsertOutcome::Stored(c)) => {
                    tracing::info!(id = c.id, kind = c.kind.as_str(), "captured clip");
                }
                Ok(InsertOutcome::Deduped(c)) => {
                    tracing::debug!(id = c.id, "clip already known; bumped");
                }
                Ok(InsertOutcome::Skipped(reason)) => {
                    tracing::debug!(reason = reason.as_str(), "clip skipped");
                }
                Err(e) => tracing::error!(error = %e, "failed to store clip"),
            });

            let _ = bridge.join();
        })
}

/// Open the history database behind the mutex the service expects.
pub fn open_store(path: &std::path::Path) -> pasteport_core::Result<Arc<Mutex<Store>>> {
    Ok(Arc::new(Mutex::new(Store::open(path)?)))
}
