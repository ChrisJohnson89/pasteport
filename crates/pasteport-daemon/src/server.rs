use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::protocol::{encode_line, Request, Response};
use crate::service::Service;

/// Refuse protocol lines longer than this. Clipboard payloads travel as
/// base64 in responses, not requests, so requests are always small.
const MAX_REQUEST_BYTES: usize = 1024 * 1024;

/// Bind the control socket, replacing a stale one left by a crash.
///
/// A socket file whose daemon is gone cannot be bound over, but it also cannot
/// be blindly deleted: doing so would let a second daemon steal the socket from
/// a healthy first one. So we probe it, and only unlink when nothing answers.
pub fn bind(socket: &Path) -> anyhow::Result<UnixListener> {
    match UnixListener::bind(socket) {
        Ok(listener) => {
            restrict(socket)?;
            Ok(listener)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if UnixStream::connect(socket).is_ok() {
                anyhow::bail!(
                    "another Pasteport daemon is already listening on {}",
                    socket.display()
                );
            }
            tracing::warn!(socket = %socket.display(), "removing stale socket");
            std::fs::remove_file(socket)?;
            let listener = UnixListener::bind(socket)?;
            restrict(socket)?;
            Ok(listener)
        }
        Err(e) => Err(e).map_err(|e| {
            anyhow::anyhow!("could not bind control socket {}: {e}", socket.display())
        }),
    }
}

fn restrict(socket: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(socket)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(socket, perms)
}

/// Cap on simultaneous client connections.
///
/// A handful of front ends plus a CLI invocation is the real workload; anything
/// beyond this is a runaway client, and refusing is better than spawning threads
/// without bound.
const MAX_CONNECTIONS: usize = 64;

/// Accept connections until the service is asked to shut down.
///
/// One thread per connection. Clients hold the socket open for the length of a
/// session — the GUIs keep theirs for as long as their window is up — so
/// handling connections in sequence would let one idle client block every other.
/// The store's mutex already serializes the actual database work, so the threads
/// cost little beyond a stack.
pub fn serve(service: Arc<Service>, listener: UnixListener, socket: &Path) -> anyhow::Result<()> {
    tracing::info!("control socket ready");
    let live = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    for incoming in listener.incoming() {
        if service.is_shutting_down() {
            break;
        }
        match incoming {
            Ok(stream) => {
                if live.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
                    tracing::warn!("connection limit reached; refusing a client");
                    // Say why, rather than dropping the socket silently.
                    let mut stream = stream;
                    let _ = stream.write_all(
                        b"{\"result\":\"error\",\"message\":\"too many connections\"}\n",
                    );
                    continue;
                }

                let conn_service = Arc::clone(&service);
                let conn_socket = socket.to_path_buf();
                let conn_live = Arc::clone(&live);
                live.fetch_add(1, Ordering::Relaxed);

                let spawned = std::thread::Builder::new()
                    .name("pasteport-conn".into())
                    .spawn(move || {
                        if let Err(e) = handle_connection(&conn_service, stream) {
                            // A client that hangs up mid-request is routine.
                            tracing::debug!(error = %e, "client connection ended");
                        }
                        conn_live.fetch_sub(1, Ordering::Relaxed);

                        // A `Shutdown` handled on this thread leaves the accept
                        // loop blocked; dial our own socket so it wakes up and
                        // sees the flag.
                        if conn_service.is_shutting_down() {
                            let _ = UnixStream::connect(&conn_socket);
                        }
                    });

                if let Err(e) = spawned {
                    tracing::error!(error = %e, "could not spawn a connection handler");
                    live.fetch_sub(1, Ordering::Relaxed);
                }
            }
            Err(e) => tracing::warn!(error = %e, "failed to accept a connection"),
        }
    }
    tracing::info!("control socket closed");
    Ok(())
}

/// Serve every request on one connection, so a client can hold the socket open.
fn handle_connection(service: &Service, stream: UnixStream) -> std::io::Result<()> {
    let mut writer = stream.try_clone()?;
    let reader = BufReader::new(stream);

    for line in reader.split(b'\n') {
        let line = line?;
        if line.len() > MAX_REQUEST_BYTES {
            let resp = Response::error("request too large");
            writer.write_all(encode_line(&resp).unwrap_or_default().as_bytes())?;
            writer.flush()?;
            break;
        }
        let trimmed = line.strip_suffix(b"\r").unwrap_or(&line);
        if trimmed.is_empty() {
            continue;
        }

        let response = match serde_json::from_slice::<Request>(trimmed) {
            Ok(req) => {
                tracing::trace!(?req, "request");
                service.handle(req)
            }
            // Deliberately echoes the parse error: the clients are all ours,
            // and a developer debugging the protocol needs to know what broke.
            Err(e) => Response::error(format!("could not parse request: {e}")),
        };

        let encoded = encode_line(&response)
            .unwrap_or_else(|e| format!("{{\"result\":\"error\",\"message\":\"{e}\"}}\n"));
        writer.write_all(encoded.as_bytes())?;
        writer.flush()?;

        if service.is_shutting_down() {
            break;
        }
    }
    Ok(())
}

/// Remove the socket file on a clean exit.
pub fn cleanup(socket: &Path) {
    match std::fs::remove_file(socket) {
        Ok(()) => tracing::debug!("control socket removed"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(error = %e, "could not remove control socket"),
    }
}

/// A minimal blocking client. Used by the CLI and by the integration tests.
#[derive(Debug)]
pub struct Client {
    stream: UnixStream,
}

impl Client {
    pub fn connect(socket: &Path) -> std::io::Result<Client> {
        Ok(Client {
            stream: UnixStream::connect(socket)?,
        })
    }

    /// Send one request and read one response.
    pub fn request(&mut self, req: &Request) -> anyhow::Result<Response> {
        let line = encode_line(req)?;
        self.stream.write_all(line.as_bytes())?;
        self.stream.flush()?;

        let mut reader = BufReader::new(&self.stream);
        let mut buf = String::new();
        let read = reader.read_line(&mut buf)?;
        if read == 0 {
            anyhow::bail!("daemon closed the connection without replying");
        }
        Ok(serde_json::from_str(buf.trim())?)
    }
}

/// True when a daemon is listening on `socket`.
pub fn is_running(socket: &Path) -> bool {
    UnixStream::connect(socket).is_ok()
}

/// Set by the signal handler. A plain static atomic, because storing to one is
/// about the only thing a signal handler is allowed to do.
static SIGNALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn handle_signal(_sig: i32) {
    SIGNALLED.store(true, Ordering::SeqCst);
}

/// Make Ctrl-C and `SIGTERM` shut the daemon down cleanly.
///
/// The handler itself only flips an atomic. A small watcher thread does the
/// real work: it flags the service and then dials the socket, because
/// `accept()` is blocking and will not otherwise notice that we want to stop.
pub fn install_signal_handlers(service: &Arc<Service>, socket: &Path) {
    let flag = service.shutdown_handle();
    let socket = socket.to_path_buf();

    // Safety: `signal` is async-signal-safe to call here, and the handler does
    // nothing but an atomic store.
    unsafe {
        libc::signal(
            libc::SIGINT,
            handle_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            handle_signal as *const () as libc::sighandler_t,
        );
        // Writing to a socket whose peer vanished must not kill the daemon.
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }

    std::thread::Builder::new()
        .name("pasteport-signals".into())
        .spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                if SIGNALLED.load(Ordering::SeqCst) {
                    tracing::info!("signal received; shutting down");
                    flag.store(true, Ordering::Relaxed);
                    // Unblock the accept loop so it can see the flag.
                    let _ = UnixStream::connect(&socket);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
        })
        .expect("failed to spawn signal watcher thread");
}

#[cfg(test)]
mod tests {
    use super::*;
    use pasteport_clipboard::{ClipboardBackend, Payload, Result as ClipResult};
    use pasteport_core::{Config, NewClip, Store};
    use pasteport_license::Licensing;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    struct NullClipboard;

    impl ClipboardBackend for NullClipboard {
        fn name(&self) -> &'static str {
            "null"
        }
        fn poll(&mut self) -> ClipResult<Option<NewClip>> {
            Ok(None)
        }
        fn read_now(&mut self) -> ClipResult<Option<NewClip>> {
            Ok(None)
        }
        fn write(&mut self, _payload: Payload<'_>) -> ClipResult<()> {
            Ok(())
        }
    }

    fn spawn_daemon() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        std::thread::JoinHandle<()>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");

        let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
        {
            let s = store.lock().unwrap();
            s.insert(&NewClip::text("seeded clip"), &Config::default())
                .unwrap();
        }
        let service = Arc::new(Service::new(
            store,
            Config::default(),
            Licensing::unconfigured(),
            Box::new(NullClipboard),
            dir.path().to_path_buf(),
        ));

        let listener = bind(&socket).unwrap();
        let socket_for_serve = socket.clone();
        let handle = std::thread::spawn(move || {
            serve(service, listener, &socket_for_serve).unwrap();
        });
        (dir, socket, handle)
    }

    fn shutdown(socket: &Path, handle: std::thread::JoinHandle<()>) {
        let mut client = Client::connect(socket).unwrap();
        client.request(&Request::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn round_trips_a_request_over_the_socket() {
        let (_dir, socket, handle) = spawn_daemon();

        let mut client = Client::connect(&socket).unwrap();
        let resp = client.request(&Request::Ping).unwrap();
        assert!(matches!(resp, Response::Pong { .. }));

        let resp = client
            .request(&Request::List {
                limit: 10,
                offset: 0,
                kind: None,
            })
            .unwrap();
        assert_eq!(crate::service::clips_of(&resp).unwrap().len(), 1);

        shutdown(&socket, handle);
    }

    #[test]
    fn serves_several_requests_on_one_connection() {
        let (_dir, socket, handle) = spawn_daemon();
        let mut client = Client::connect(&socket).unwrap();
        for _ in 0..5 {
            assert!(matches!(
                client.request(&Request::Ping).unwrap(),
                Response::Pong { .. }
            ));
        }
        shutdown(&socket, handle);
    }

    #[test]
    fn malformed_input_gets_an_error_not_a_dropped_connection() {
        let (_dir, socket, handle) = spawn_daemon();

        let mut stream = UnixStream::connect(&socket).unwrap();
        stream.write_all(b"this is not json\n").unwrap();
        stream.flush().unwrap();

        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let resp: Response = serde_json::from_str(line.trim()).unwrap();
        assert!(resp.is_error());

        // The connection still works afterwards.
        stream.write_all(b"{\"op\":\"ping\"}\n").unwrap();
        stream.flush().unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(matches!(
            serde_json::from_str::<Response>(line.trim()).unwrap(),
            Response::Pong { .. }
        ));

        shutdown(&socket, handle);
    }

    #[test]
    fn blank_lines_are_ignored() {
        let (_dir, socket, handle) = spawn_daemon();
        let mut stream = UnixStream::connect(&socket).unwrap();
        stream.write_all(b"\n\n{\"op\":\"ping\"}\n").unwrap();
        stream.flush().unwrap();

        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(matches!(
            serde_json::from_str::<Response>(line.trim()).unwrap(),
            Response::Pong { .. }
        ));
        shutdown(&socket, handle);
    }

    #[test]
    fn socket_is_owner_only() {
        let (_dir, socket, handle) = spawn_daemon();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "the control socket must not be world-accessible"
        );
        shutdown(&socket, handle);
    }

    #[test]
    fn is_running_detects_a_live_daemon() {
        let (dir, socket, handle) = spawn_daemon();
        assert!(is_running(&socket));
        assert!(!is_running(&dir.path().join("nonexistent.sock")));
        shutdown(&socket, handle);
    }

    #[test]
    fn a_stale_socket_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("stale.sock");

        // Leave a socket file behind with nothing listening, as a crash would.
        let listener = UnixListener::bind(&socket).unwrap();
        drop(listener);
        assert!(socket.exists());

        let listener = bind(&socket).expect("a stale socket should be reclaimed");
        drop(listener);
    }

    #[test]
    fn a_live_socket_is_not_stolen() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("live.sock");
        let _first = bind(&socket).unwrap();

        let err = bind(&socket).expect_err("a second daemon must refuse to start");
        assert!(err.to_string().contains("already listening"));
    }

    #[test]
    fn cleanup_removes_the_socket_and_tolerates_a_missing_one() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("cleanup.sock");
        let listener = bind(&socket).unwrap();
        drop(listener);

        cleanup(&socket);
        assert!(!socket.exists());
        cleanup(&socket); // second call must not panic
    }
}
