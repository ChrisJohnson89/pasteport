use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pasteport_core::NewClip;

use crate::{ClipboardBackend, Result};

/// Polls a backend and hands every new clip to a callback.
///
/// Kept separate from the backends so the polling policy, backoff, and stop
/// signal are written once rather than per platform.
#[derive(Debug)]
pub struct Watcher {
    backend: Box<dyn ClipboardBackend>,
    interval: Duration,
    stop: Arc<AtomicBool>,
    /// Consecutive backend failures, used to back off instead of spinning on a
    /// clipboard that is wedged (a dead X11 selection owner, say).
    consecutive_errors: u32,
}

impl Watcher {
    pub fn new(backend: Box<dyn ClipboardBackend>, interval: Duration) -> Self {
        Watcher {
            backend,
            interval: interval.max(Duration::from_millis(50)),
            stop: Arc::new(AtomicBool::new(false)),
            consecutive_errors: 0,
        }
    }

    /// A handle that stops [`run`](Self::run) from another thread or a signal
    /// handler.
    pub fn stop_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    /// Read the clipboard once, whether or not it changed.
    pub fn read_now(&mut self) -> Result<Option<NewClip>> {
        self.backend.read_now()
    }

    pub fn write(&mut self, payload: crate::Payload<'_>) -> Result<()> {
        self.backend.write(payload)
    }

    /// One poll cycle. Returns the new clip, if any.
    pub fn poll(&mut self) -> Result<Option<NewClip>> {
        match self.backend.poll() {
            Ok(clip) => {
                self.consecutive_errors = 0;
                Ok(clip)
            }
            Err(e) => {
                self.consecutive_errors = self.consecutive_errors.saturating_add(1);
                Err(e)
            }
        }
    }

    /// How long to wait before the next poll, doubling up to ~10s while the
    /// backend keeps failing.
    pub fn current_interval(&self) -> Duration {
        if self.consecutive_errors == 0 {
            return self.interval;
        }
        let factor = 1u32 << self.consecutive_errors.min(5);
        (self.interval * factor).min(Duration::from_secs(10))
    }

    /// Poll until the stop handle is set, invoking `on_clip` for each new clip.
    ///
    /// Backend errors are logged and retried with backoff rather than ending
    /// the loop: a clipboard that fails once (an app dying mid-copy) should not
    /// take the daemon down with it.
    pub fn run<F>(&mut self, mut on_clip: F)
    where
        F: FnMut(NewClip),
    {
        tracing::info!(backend = self.backend.name(), "clipboard watcher started");
        while !self.stop.load(Ordering::Relaxed) {
            match self.poll() {
                Ok(Some(clip)) => on_clip(clip),
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "clipboard poll failed; backing off"),
            }
            // Sleep in slices so a stop signal is honoured promptly even when
            // the backoff interval is long.
            let deadline = std::time::Instant::now() + self.current_interval();
            while std::time::Instant::now() < deadline {
                if self.stop.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50).min(self.interval));
            }
        }
        tracing::info!("clipboard watcher stopped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Payload;

    /// A scripted backend: yields queued clips, then optionally fails forever.
    #[derive(Debug)]
    struct FakeBackend {
        queue: Vec<Option<NewClip>>,
        fail_after_queue: bool,
        writes: Vec<String>,
    }

    impl ClipboardBackend for FakeBackend {
        fn name(&self) -> &'static str {
            "fake"
        }

        fn poll(&mut self) -> Result<Option<NewClip>> {
            if self.queue.is_empty() {
                if self.fail_after_queue {
                    return Err(crate::Error::ClipboardUnavailable);
                }
                return Ok(None);
            }
            Ok(self.queue.remove(0))
        }

        fn read_now(&mut self) -> Result<Option<NewClip>> {
            self.poll()
        }

        fn write(&mut self, payload: Payload<'_>) -> Result<()> {
            if let Payload::Text(t) = payload {
                self.writes.push(t.to_string());
            }
            Ok(())
        }
    }

    fn fake(queue: Vec<Option<NewClip>>, fail_after_queue: bool) -> Box<FakeBackend> {
        Box::new(FakeBackend {
            queue,
            fail_after_queue,
            writes: Vec::new(),
        })
    }

    #[test]
    fn forwards_new_clips_and_stops_on_signal() {
        let backend = fake(
            vec![Some(NewClip::text("one")), None, Some(NewClip::text("two"))],
            false,
        );
        let mut watcher = Watcher::new(backend, Duration::from_millis(50));
        let stop = watcher.stop_handle();

        let mut seen = Vec::new();
        // Stop after the queue drains: three polls' worth.
        let mut polls = 0;
        loop {
            if let Some(clip) = watcher.poll().unwrap() {
                seen.push(clip.text.unwrap_or_default());
            }
            polls += 1;
            if polls == 3 {
                stop.store(true, Ordering::Relaxed);
                break;
            }
        }
        assert_eq!(seen, vec!["one", "two"]);
    }

    #[test]
    fn run_exits_promptly_when_stopped() {
        let backend = fake(vec![Some(NewClip::text("only"))], false);
        let mut watcher = Watcher::new(backend, Duration::from_millis(50));
        let stop = watcher.stop_handle();

        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_clone = std::sync::Arc::clone(&seen);
        let handle = std::thread::spawn(move || {
            watcher.run(|clip| {
                seen_clone
                    .lock()
                    .unwrap()
                    .push(clip.text.unwrap_or_default());
            });
        });

        std::thread::sleep(Duration::from_millis(200));
        stop.store(true, Ordering::Relaxed);
        handle
            .join()
            .expect("watcher thread should exit after stop");

        assert_eq!(seen.lock().unwrap().as_slice(), ["only".to_string()]);
    }

    #[test]
    fn backs_off_on_repeated_errors_and_recovers() {
        let backend = fake(vec![], true);
        let mut watcher = Watcher::new(backend, Duration::from_millis(100));
        assert_eq!(watcher.current_interval(), Duration::from_millis(100));

        for _ in 0..3 {
            assert!(watcher.poll().is_err());
        }
        assert!(
            watcher.current_interval() > Duration::from_millis(100),
            "repeated failures must widen the interval"
        );

        // Backoff is capped so a wedged clipboard is still retried.
        for _ in 0..50 {
            let _ = watcher.poll();
        }
        assert!(watcher.current_interval() <= Duration::from_secs(10));
    }

    #[test]
    fn interval_has_a_floor() {
        let watcher = Watcher::new(fake(vec![], false), Duration::from_millis(1));
        assert_eq!(watcher.current_interval(), Duration::from_millis(50));
    }

    #[test]
    fn write_reaches_the_backend() {
        let mut watcher = Watcher::new(fake(vec![], false), Duration::from_millis(50));
        watcher.write(Payload::Text("hello")).unwrap();
    }
}
