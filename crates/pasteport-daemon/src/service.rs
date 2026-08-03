use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use base64::Engine as _;
use pasteport_clipboard::{ClipboardBackend, Payload};
use pasteport_core::{Clip, Config, InsertOutcome, NewClip, Store};

use crate::protocol::{Request, Response, StatusReport};

/// Upper bound on any `limit` a client can ask for, so one bad request cannot
/// make the daemon materialize the whole history.
const MAX_LIMIT: usize = 1_000;

/// Everything the daemon does, independent of transport.
///
/// The socket server calls [`handle`](Self::handle); so do the tests, with a
/// fake clipboard. Keeping the transport out of here is what makes the
/// behaviour testable without opening a socket.
pub struct Service {
    store: Arc<Mutex<Store>>,
    config: Config,
    /// Separate clipboard handle used for writes, so a `Copy` request never
    /// waits on the watcher thread's poll.
    writer: Mutex<Box<dyn ClipboardBackend>>,
    backend_name: String,
    started: Instant,
    data_dir: PathBuf,
    shutdown: Arc<AtomicBool>,
}

impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Service")
            .field("backend", &self.backend_name)
            .field("data_dir", &self.data_dir)
            .finish_non_exhaustive()
    }
}

impl Service {
    pub fn new(
        store: Arc<Mutex<Store>>,
        config: Config,
        writer: Box<dyn ClipboardBackend>,
        data_dir: PathBuf,
    ) -> Self {
        let backend_name = writer.name().to_string();
        Service {
            store,
            config,
            writer: Mutex::new(writer),
            backend_name,
            started: Instant::now(),
            data_dir,
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Flag the socket server and watcher check to stop.
    pub fn shutdown_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shutdown)
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }

    pub fn store(&self) -> Arc<Mutex<Store>> {
        Arc::clone(&self.store)
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Store a clip the watcher just observed.
    ///
    /// Returns the outcome so the caller can log it. Prunes opportunistically
    /// on new inserts rather than on a timer: the only moment the history can
    /// exceed its limits is right after something was added.
    pub fn ingest(&self, clip: &NewClip) -> pasteport_core::Result<InsertOutcome> {
        let store = self.store.lock().expect("store mutex poisoned");
        let outcome = store.insert(clip, &self.config)?;
        if matches!(outcome, InsertOutcome::Stored(_)) {
            match store.prune(&self.config) {
                Ok(n) if n > 0 => tracing::debug!(pruned = n, "retention policy applied"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "prune failed"),
            }
        }
        Ok(outcome)
    }

    /// Dispatch one request.
    ///
    /// Every arm returns a `Response` rather than propagating an error, so a
    /// bad request from one client never takes the daemon down.
    /// Pasteport is free: every request is served, with no entitlement check
    /// of any kind. There is deliberately no gate here to reintroduce.
    pub fn handle(&self, req: Request) -> Response {
        match req {
            Request::Ping => Response::Pong {
                version: pasteport_core::VERSION.to_string(),
            },
            Request::Status => self.status(),

            Request::List {
                limit,
                offset,
                kind,
            } => {
                let limit = clamp_limit(limit);
                let store = self.lock();
                let result = match kind {
                    Some(k) => store.by_kind(k, limit),
                    None => store.recent(limit, offset),
                };
                match result {
                    Ok(clips) => Response::Clips { clips },
                    Err(e) => Response::error(e),
                }
            }

            Request::Search { query, limit } => {
                match self.lock().search(&query, clamp_limit(limit)) {
                    Ok(clips) => Response::Clips { clips },
                    Err(e) => Response::error(e),
                }
            }

            Request::Get { id } => match self.lock().get(id) {
                Ok(clip) => Response::Clip {
                    clip: Box::new(clip),
                },
                Err(e) => Response::error(e),
            },

            Request::GetBytes { id } => match self.lock().clip_bytes(id) {
                Ok(bytes) => Response::Bytes {
                    base64: bytes.map(|b| base64::engine::general_purpose::STANDARD.encode(b)),
                },
                Err(e) => Response::error(e),
            },

            Request::Copy { id } => self.copy_to_clipboard(id),

            Request::Pin { id, pinned } => match self.lock().set_pinned(id, pinned) {
                Ok(clip) => Response::Clip {
                    clip: Box::new(clip),
                },
                Err(e) => Response::error(e),
            },

            Request::Delete { id } => match self.lock().delete(id) {
                Ok(()) => Response::Ok,
                Err(e) => Response::error(e),
            },

            Request::Clear { include_pinned } => match self.lock().clear(include_pinned) {
                Ok(count) => Response::Count { count },
                Err(e) => Response::error(e),
            },

            Request::CaptureNow => self.capture_now(),

            Request::Prune => match self.lock().prune(&self.config) {
                Ok(count) => Response::Count { count },
                Err(e) => Response::error(e),
            },

            Request::Pinboards => match self.lock().list_pinboards() {
                Ok(pinboards) => Response::Pinboards { pinboards },
                Err(e) => Response::error(e),
            },

            Request::PinboardCreate { name } => match self.lock().create_pinboard(&name) {
                Ok(_) => Response::Ok,
                Err(e) => Response::error(e),
            },

            Request::PinboardDelete { name } => match self.lock().delete_pinboard(&name) {
                Ok(()) => Response::Ok,
                Err(e) => Response::error(e),
            },

            Request::PinboardAdd { name, id } => match self.lock().add_to_pinboard(&name, id) {
                Ok(()) => Response::Ok,
                Err(e) => Response::error(e),
            },

            Request::PinboardRemove { name, id } => {
                match self.lock().remove_from_pinboard(&name, id) {
                    Ok(()) => Response::Ok,
                    Err(e) => Response::error(e),
                }
            }

            Request::PinboardClips { name, limit } => {
                match self.lock().pinboard_clips(&name, clamp_limit(limit)) {
                    Ok(clips) => Response::Clips { clips },
                    Err(e) => Response::error(e),
                }
            }

            Request::Shutdown => {
                tracing::info!("shutdown requested");
                self.shutdown.store(true, Ordering::Relaxed);
                Response::Ok
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().expect("store mutex poisoned")
    }

    fn status(&self) -> Response {
        let stats = match self.lock().stats() {
            Ok(s) => s,
            Err(e) => return Response::error(e),
        };
        Response::Status(Box::new(StatusReport {
            version: pasteport_core::VERSION.to_string(),
            backend: self.backend_name.clone(),
            uptime_secs: self.started.elapsed().as_secs(),
            poll_interval_ms: self.config.poll_interval_ms,
            stats,
            data_dir: self.data_dir.display().to_string(),
        }))
    }

    /// Put a stored clip back on the system clipboard.
    fn copy_to_clipboard(&self, id: i64) -> Response {
        // Read everything we need before touching the clipboard, and drop the
        // store lock before the write: helper subprocesses on Linux can take
        // milliseconds, and the watcher thread should not stall behind them.
        let (clip, bytes) = {
            let store = self.lock();
            let clip = match store.get(id) {
                Ok(c) => c,
                Err(e) => return Response::error(e),
            };
            let bytes = if clip.text.is_none() {
                match store.clip_bytes(id) {
                    Ok(b) => b,
                    Err(e) => return Response::error(e),
                }
            } else {
                None
            };
            (clip, bytes)
        };

        let write_result = {
            let mut writer = self.writer.lock().expect("writer mutex poisoned");
            match (&clip.text, &bytes) {
                (Some(text), _) => writer.write(Payload::Text(text)),
                (None, Some(bytes)) => writer.write(Payload::Image {
                    bytes,
                    mime: &clip.mime,
                }),
                (None, None) => {
                    return Response::error("clip has no content to place on the clipboard")
                }
            }
        };
        if let Err(e) = write_result {
            return Response::error(e);
        }

        match self.lock().touch(id) {
            Ok(clip) => Response::Clip {
                clip: Box::new(clip),
            },
            Err(e) => Response::error(e),
        }
    }

    /// Read the clipboard right now and store it, whether or not it changed.
    fn capture_now(&self) -> Response {
        let clip = {
            let mut writer = self.writer.lock().expect("writer mutex poisoned");
            match writer.read_now() {
                Ok(Some(clip)) => clip,
                Ok(None) => return Response::error("the clipboard is empty"),
                Err(e) => return Response::error(e),
            }
        };
        match self.ingest(&clip) {
            Ok(InsertOutcome::Stored(c)) | Ok(InsertOutcome::Deduped(c)) => {
                Response::Clip { clip: Box::new(c) }
            }
            Ok(InsertOutcome::Skipped(reason)) => {
                Response::error(format!("clipboard contents skipped: {}", reason.as_str()))
            }
            Err(e) => Response::error(e),
        }
    }
}

fn clamp_limit(limit: usize) -> usize {
    limit.clamp(1, MAX_LIMIT)
}

/// Convenience for clients: pull the clips out of a `Clips` response.
pub fn clips_of(response: &Response) -> Option<&[Clip]> {
    match response {
        Response::Clips { clips } => Some(clips),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pasteport_clipboard::Result as ClipResult;

    /// A clipboard we can script and inspect.
    #[derive(Debug, Default)]
    struct FakeClipboard {
        current: Option<NewClip>,
        writes: Vec<String>,
        image_writes: Vec<(Vec<u8>, String)>,
    }

    impl ClipboardBackend for FakeClipboard {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn poll(&mut self) -> ClipResult<Option<NewClip>> {
            Ok(self.current.take())
        }
        fn read_now(&mut self) -> ClipResult<Option<NewClip>> {
            Ok(self.current.clone())
        }
        fn write(&mut self, payload: Payload<'_>) -> ClipResult<()> {
            match payload {
                Payload::Text(t) => self.writes.push(t.to_string()),
                Payload::Image { bytes, mime } => {
                    self.image_writes.push((bytes.to_vec(), mime.to_string()))
                }
            }
            Ok(())
        }
    }

    /// A clipboard whose writes always fail.
    #[derive(Debug)]
    struct BrokenClipboard;

    impl ClipboardBackend for BrokenClipboard {
        fn name(&self) -> &'static str {
            "broken"
        }
        fn poll(&mut self) -> ClipResult<Option<NewClip>> {
            Ok(None)
        }
        fn read_now(&mut self) -> ClipResult<Option<NewClip>> {
            Err(pasteport_clipboard::Error::ClipboardUnavailable)
        }
        fn write(&mut self, _payload: Payload<'_>) -> ClipResult<()> {
            Err(pasteport_clipboard::Error::ClipboardUnavailable)
        }
    }

    fn service_with(backend: Box<dyn ClipboardBackend>) -> Service {
        let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
        Service::new(
            store,
            Config::default(),
            backend,
            PathBuf::from("/tmp/pasteport-test"),
        )
    }

    fn service() -> Service {
        service_with(Box::new(FakeClipboard::default()))
    }

    fn seed(svc: &Service, texts: &[&str]) -> Vec<i64> {
        texts
            .iter()
            .map(|t| match svc.ingest(&NewClip::text(*t)).unwrap() {
                InsertOutcome::Stored(c) | InsertOutcome::Deduped(c) => c.id,
                InsertOutcome::Skipped(r) => panic!("skipped: {}", r.as_str()),
            })
            .collect()
    }

    #[test]
    fn ping_reports_the_version() {
        let svc = service();
        assert_eq!(
            svc.handle(Request::Ping),
            Response::Pong {
                version: pasteport_core::VERSION.to_string()
            }
        );
    }

    #[test]
    fn status_reports_backend_and_stats() {
        let svc = service();
        seed(&svc, &["a", "b"]);
        let Response::Status(report) = svc.handle(Request::Status) else {
            panic!("expected a status report");
        };
        assert_eq!(report.backend, "fake");
        assert_eq!(report.stats.total_clips, 2);
    }

    #[test]
    fn list_and_search_return_clips() {
        let svc = service();
        seed(&svc, &["alpha text", "beta text", "https://example.com"]);

        let listed = svc.handle(Request::List {
            limit: 50,
            offset: 0,
            kind: None,
        });
        assert_eq!(clips_of(&listed).unwrap().len(), 3);

        let links = svc.handle(Request::List {
            limit: 50,
            offset: 0,
            kind: Some(pasteport_core::ClipKind::Link),
        });
        assert_eq!(clips_of(&links).unwrap().len(), 1);

        let found = svc.handle(Request::Search {
            query: "beta".into(),
            limit: 10,
        });
        assert_eq!(clips_of(&found).unwrap().len(), 1);
    }

    #[test]
    fn limits_are_clamped_rather_than_trusted() {
        let svc = service();
        seed(&svc, &["one"]);
        // Zero would produce an empty page; huge would try to load everything.
        assert_eq!(clamp_limit(0), 1);
        assert_eq!(clamp_limit(usize::MAX), MAX_LIMIT);

        let resp = svc.handle(Request::List {
            limit: 0,
            offset: 0,
            kind: None,
        });
        assert_eq!(clips_of(&resp).unwrap().len(), 1);
    }

    #[test]
    fn copy_writes_text_to_the_clipboard_and_bumps_recency() {
        let svc = service();
        let ids = seed(&svc, &["first", "second"]);

        let resp = svc.handle(Request::Copy { id: ids[0] });
        let Response::Clip { clip } = resp else {
            panic!("expected the clip back")
        };
        assert_eq!(clip.use_count, 2, "copying counts as a use");

        // It is now the most recent.
        let listed = svc.handle(Request::List {
            limit: 10,
            offset: 0,
            kind: None,
        });
        assert_eq!(clips_of(&listed).unwrap()[0].id, ids[0]);
    }

    #[test]
    fn copy_writes_image_bytes_for_binary_clips() {
        let svc = service();
        let payload = vec![0x89, 0x50, 0x4e, 0x47];
        let stored = svc
            .ingest(&NewClip::image(payload.clone(), "image/png"))
            .unwrap();
        let id = match stored {
            InsertOutcome::Stored(c) => c.id,
            other => panic!("expected a stored clip, got {other:?}"),
        };

        assert!(!svc.handle(Request::Copy { id }).is_error());
    }

    #[test]
    fn copy_reports_a_clipboard_failure_instead_of_lying() {
        let svc = service_with(Box::new(BrokenClipboard));
        let ids = seed(&svc, &["text"]);
        let resp = svc.handle(Request::Copy { id: ids[0] });
        assert!(resp.is_error(), "a failed clipboard write must be reported");

        // And the clip was not marked as used.
        let Response::Clip { clip } = svc.handle(Request::Get { id: ids[0] }) else {
            panic!("expected the clip")
        };
        assert_eq!(clip.use_count, 1);
    }

    #[test]
    fn get_bytes_encodes_base64_and_is_none_for_text() {
        let svc = service();
        let payload = vec![0, 1, 2];
        let id = match svc
            .ingest(&NewClip::image(payload.clone(), "image/png"))
            .unwrap()
        {
            InsertOutcome::Stored(c) => c.id,
            other => panic!("{other:?}"),
        };
        let Response::Bytes { base64 } = svc.handle(Request::GetBytes { id }) else {
            panic!("expected bytes")
        };
        assert_eq!(base64.as_deref(), Some("AAEC"));

        let text_ids = seed(&svc, &["plain"]);
        let Response::Bytes { base64 } = svc.handle(Request::GetBytes { id: text_ids[0] }) else {
            panic!("expected bytes")
        };
        assert_eq!(base64, None);
    }

    #[test]
    fn pin_delete_and_clear_behave() {
        let svc = service();
        let ids = seed(&svc, &["keep", "drop"]);

        assert!(!svc
            .handle(Request::Pin {
                id: ids[0],
                pinned: true
            })
            .is_error());
        assert!(!svc.handle(Request::Delete { id: ids[1] }).is_error());

        let Response::Count { count } = svc.handle(Request::Clear {
            include_pinned: false,
        }) else {
            panic!("expected a count")
        };
        assert_eq!(count, 0, "the only remaining clip is pinned");

        let Response::Count { count } = svc.handle(Request::Clear {
            include_pinned: true,
        }) else {
            panic!("expected a count")
        };
        assert_eq!(count, 1);
    }

    #[test]
    fn pinboard_operations_work_end_to_end() {
        let svc = service();
        let ids = seed(&svc, &["snippet"]);

        assert!(!svc
            .handle(Request::PinboardCreate {
                name: "work".into()
            })
            .is_error());
        assert!(!svc
            .handle(Request::PinboardAdd {
                name: "work".into(),
                id: ids[0]
            })
            .is_error());

        let Response::Pinboards { pinboards } = svc.handle(Request::Pinboards) else {
            panic!("expected pinboards")
        };
        assert_eq!(pinboards.len(), 1);
        assert_eq!(pinboards[0].clip_count, 1);

        let listed = svc.handle(Request::PinboardClips {
            name: "work".into(),
            limit: 10,
        });
        assert_eq!(clips_of(&listed).unwrap().len(), 1);

        assert!(!svc
            .handle(Request::PinboardRemove {
                name: "work".into(),
                id: ids[0]
            })
            .is_error());
        assert!(!svc
            .handle(Request::PinboardDelete {
                name: "work".into()
            })
            .is_error());
        assert!(svc
            .handle(Request::PinboardClips {
                name: "work".into(),
                limit: 10
            })
            .is_error());
    }

    #[test]
    fn missing_ids_produce_errors_not_panics() {
        let svc = service();
        for req in [
            Request::Get { id: 999 },
            Request::GetBytes { id: 999 },
            Request::Copy { id: 999 },
            Request::Pin {
                id: 999,
                pinned: true,
            },
            Request::Delete { id: 999 },
            Request::PinboardAdd {
                name: "nope".into(),
                id: 999,
            },
        ] {
            assert!(
                svc.handle(req.clone()).is_error(),
                "{req:?} should be an error"
            );
        }
    }

    #[test]
    fn capture_now_stores_the_current_clipboard() {
        let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
        let backend = FakeClipboard {
            current: Some(NewClip::text("on the clipboard")),
            ..Default::default()
        };
        let svc = Service::new(
            store,
            Config::default(),
            Box::new(backend),
            PathBuf::from("/tmp/pasteport-test"),
        );

        let Response::Clip { clip } = svc.handle(Request::CaptureNow) else {
            panic!("expected the captured clip")
        };
        assert_eq!(clip.text.as_deref(), Some("on the clipboard"));
    }

    #[test]
    fn capture_now_on_an_empty_clipboard_is_an_error() {
        let svc = service();
        assert!(svc.handle(Request::CaptureNow).is_error());
    }

    #[test]
    fn concealed_clips_are_never_ingested() {
        let svc = service();
        let outcome = svc
            .ingest(&NewClip::text("hunter2").concealed(true))
            .unwrap();
        assert!(matches!(outcome, InsertOutcome::Skipped(_)));
        let listed = svc.handle(Request::List {
            limit: 10,
            offset: 0,
            kind: None,
        });
        assert!(clips_of(&listed).unwrap().is_empty());
    }

    #[test]
    fn ingest_prunes_to_the_configured_cap() {
        let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
        let config = Config {
            max_items: 3,
            retention_days: 0,
            ..Config::default()
        };
        let svc = Service::new(
            store,
            config,
            Box::new(FakeClipboard::default()),
            PathBuf::from("/tmp/pasteport-test"),
        );

        for i in 0..10 {
            svc.ingest(&NewClip::text(format!("clip {i}"))).unwrap();
        }
        let listed = svc.handle(Request::List {
            limit: 100,
            offset: 0,
            kind: None,
        });
        assert_eq!(
            clips_of(&listed).unwrap().len(),
            3,
            "history must respect max_items"
        );
    }

    #[test]
    fn shutdown_sets_the_flag() {
        let svc = service();
        assert!(!svc.is_shutting_down());
        assert_eq!(svc.handle(Request::Shutdown), Response::Ok);
        assert!(svc.is_shutting_down());
    }
}
