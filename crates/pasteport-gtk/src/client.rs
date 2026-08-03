//! Thin wrapper over the daemon protocol for the GTK UI.
//!
//! Reconnects on demand rather than holding a socket open for the process
//! lifetime: the daemon may be restarted while the window is open, and the UI
//! should recover by itself instead of needing to be relaunched.

use std::path::PathBuf;

use pasteport_core::Clip;
use pasteport_daemon::protocol::{Request, Response, StatusReport};
use pasteport_daemon::Client;

/// A reconnecting client for the local daemon.
#[derive(Debug)]
pub struct Engine {
    socket: PathBuf,
}

impl Engine {
    pub fn new(socket: PathBuf) -> Self {
        Engine { socket }
    }

    pub fn socket_path(&self) -> &std::path::Path {
        &self.socket
    }

    pub fn is_running(&self) -> bool {
        pasteport_daemon::is_running(&self.socket)
    }

    /// One request, one fresh connection.
    fn call(&self, req: &Request) -> anyhow::Result<Response> {
        let mut client = Client::connect(&self.socket).map_err(|e| {
            anyhow::anyhow!(
                "cannot reach the Pasteport service at {}: {e}\nStart it with: pasteportd",
                self.socket.display()
            )
        })?;
        client.request(req)
    }

    /// Recent clips, or search results when `query` is non-empty.
    ///
    /// Collapsing these into one method keeps the UI from having to decide which
    /// request to send as the user types.
    pub fn clips(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Clip>> {
        let req = if query.trim().is_empty() {
            Request::List { limit, offset: 0, kind: None }
        } else {
            Request::Search { query: query.to_string(), limit }
        };
        match self.call(&req)? {
            Response::Clips { clips } => Ok(clips),
            Response::Error { message } => Err(anyhow::anyhow!(message)),
            other => Err(anyhow::anyhow!("unexpected response: {other:?}")),
        }
    }

    pub fn status(&self) -> anyhow::Result<StatusReport> {
        match self.call(&Request::Status)? {
            Response::Status(report) => Ok(*report),
            Response::Error { message } => Err(anyhow::anyhow!(message)),
            other => Err(anyhow::anyhow!("unexpected response: {other:?}")),
        }
    }

    pub fn copy(&self, id: i64) -> anyhow::Result<()> {
        self.expect_ok(Request::Copy { id })
    }

    pub fn set_pinned(&self, id: i64, pinned: bool) -> anyhow::Result<()> {
        self.expect_ok(Request::Pin { id, pinned })
    }

    pub fn delete(&self, id: i64) -> anyhow::Result<()> {
        self.expect_ok(Request::Delete { id })
    }

    /// Treat anything that is not an error as success.
    fn expect_ok(&self, req: Request) -> anyhow::Result<()> {
        match self.call(&req)? {
            Response::Error { message } => Err(anyhow::anyhow!(message)),
            _ => Ok(()),
        }
    }
}
