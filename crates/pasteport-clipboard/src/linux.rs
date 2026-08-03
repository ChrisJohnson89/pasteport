//! Linux backend.
//!
//! Linux has no single clipboard API: Wayland and X11 disagree, and on X11 the
//! clipboard is owned by a live process rather than the compositor. Rather than
//! link against both `libwayland` and `libX11`, Pasteport drives the standard
//! helper tools, which every desktop already ships or packages:
//!
//! * Wayland: `wl-paste` / `wl-copy` from `wl-clipboard`
//! * X11: `xclip`, or `xsel` as a fallback
//!
//! The tradeoff is one short-lived subprocess per poll instead of a library
//! call. At the default 400 ms interval that is negligible, and it keeps the
//! build free of system library headers.

use std::io::Write;
use std::process::{Command, Stdio};

use pasteport_core::{ClipKind, NewClip};

use crate::{is_concealed, ClipboardBackend, Error, Payload, Result};

/// Which helper Pasteport found on this system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxTool {
    /// `wl-clipboard`, used under Wayland. Supports type listing and images.
    WlClipboard,
    /// `xclip`, used under X11. Supports type listing and images.
    Xclip,
    /// `xsel`. Text only, no type listing.
    Xsel,
}

impl LinuxTool {
    pub fn as_str(self) -> &'static str {
        match self {
            LinuxTool::WlClipboard => "wl-clipboard",
            LinuxTool::Xclip => "xclip",
            LinuxTool::Xsel => "xsel",
        }
    }

    fn supports_images(self) -> bool {
        matches!(self, LinuxTool::WlClipboard | LinuxTool::Xclip)
    }
}

/// The Linux clipboard, via whichever helper is available.
#[derive(Debug)]
pub struct LinuxClipboard {
    tool: LinuxTool,
    /// Digest of the contents at the last poll. Linux gives us no change
    /// counter, so change detection means hashing what we read.
    last_digest: Option<String>,
}

impl LinuxClipboard {
    /// Pick a helper, preferring the one matching the current session type.
    pub fn new() -> Result<Self> {
        let tool = detect_tool().ok_or(Error::NoHelperTool)?;
        tracing::info!(tool = tool.as_str(), "using Linux clipboard helper");

        let mut cb = LinuxClipboard {
            tool,
            last_digest: None,
        };
        // Seed the digest so existing clipboard contents are not captured as a
        // fresh copy at startup.
        if let Ok(Some(clip)) = cb.read_current() {
            cb.last_digest = Some(clip.digest());
        }
        Ok(cb)
    }

    /// Force a specific helper. Mainly for tests and `--clipboard-tool`.
    pub fn with_tool(tool: LinuxTool) -> Self {
        LinuxClipboard {
            tool,
            last_digest: None,
        }
    }

    pub fn tool(&self) -> LinuxTool {
        self.tool
    }

    /// The MIME types currently offered, empty when the helper cannot list them.
    fn available_types(&self) -> Vec<String> {
        let listed = match self.tool {
            LinuxTool::WlClipboard => run_ok("wl-paste", &["--list-types"], None),
            LinuxTool::Xclip => run_ok(
                "xclip",
                &["-selection", "clipboard", "-o", "-t", "TARGETS"],
                None,
            ),
            LinuxTool::Xsel => None,
        };
        listed
            .map(|out| {
                String::from_utf8_lossy(&out)
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn read_text(&self) -> Option<String> {
        let out = match self.tool {
            LinuxTool::WlClipboard => {
                // Ask for UTF-8 explicitly, falling back to whatever is there.
                run_ok(
                    "wl-paste",
                    &["--no-newline", "--type", "text/plain;charset=utf-8"],
                    None,
                )
                .or_else(|| run_ok("wl-paste", &["--no-newline"], None))
            }
            LinuxTool::Xclip => run_ok("xclip", &["-selection", "clipboard", "-o"], None),
            LinuxTool::Xsel => run_ok("xsel", &["--clipboard", "--output"], None),
        }?;
        if out.is_empty() {
            return None;
        }
        // Lossy on purpose: a clipboard holding invalid UTF-8 is still worth
        // remembering, and the replacement characters are visible to the user.
        Some(String::from_utf8_lossy(&out).into_owned())
    }

    fn read_image(&self, mime: &str) -> Option<Vec<u8>> {
        if !self.tool.supports_images() {
            return None;
        }
        let out = match self.tool {
            LinuxTool::WlClipboard => run_ok("wl-paste", &["--type", mime], None),
            LinuxTool::Xclip => run_ok(
                "xclip",
                &["-selection", "clipboard", "-t", mime, "-o"],
                None,
            ),
            LinuxTool::Xsel => None,
        }?;
        (!out.is_empty()).then_some(out)
    }

    /// Read whatever is on the clipboard right now, without change tracking.
    fn read_current(&self) -> Result<Option<NewClip>> {
        let types = self.available_types();

        if is_concealed(&types) {
            tracing::debug!("clipboard marked as a password manager hint; not reading contents");
            let mut clip = NewClip::text("");
            clip.text = None;
            clip.mime = "application/x-concealed".to_string();
            return Ok(Some(clip.concealed(true)));
        }

        let has = |needle: &str| types.iter().any(|t| t == needle);

        for mime in ["image/png", "image/jpeg", "image/tiff"] {
            if has(mime) {
                if let Some(bytes) = self.read_image(mime) {
                    return Ok(Some(NewClip::image(bytes, mime)));
                }
            }
        }

        if has("text/uri-list") {
            if let Some(text) = self.read_text() {
                let mut clip = NewClip::text(text);
                clip.kind = ClipKind::File;
                clip.mime = "text/uri-list".to_string();
                return Ok(Some(clip));
            }
        }

        match self.read_text() {
            Some(text) => {
                let mut clip = NewClip::text(text);
                if has("text/html") {
                    clip.kind = ClipKind::RichText;
                    clip.mime = "text/html".to_string();
                }
                Ok(Some(clip))
            }
            None => Ok(None),
        }
    }
}

impl ClipboardBackend for LinuxClipboard {
    fn name(&self) -> &'static str {
        match self.tool {
            LinuxTool::WlClipboard => "linux-wl-clipboard",
            LinuxTool::Xclip => "linux-xclip",
            LinuxTool::Xsel => "linux-xsel",
        }
    }

    fn poll(&mut self) -> Result<Option<NewClip>> {
        let Some(clip) = self.read_current()? else {
            self.last_digest = None;
            return Ok(None);
        };
        let digest = clip.digest();
        if self.last_digest.as_deref() == Some(digest.as_str()) {
            return Ok(None);
        }
        self.last_digest = Some(digest);
        Ok(Some(clip))
    }

    fn read_now(&mut self) -> Result<Option<NewClip>> {
        let clip = self.read_current()?;
        self.last_digest = clip.as_ref().map(|c| c.digest());
        Ok(clip)
    }

    fn write(&mut self, payload: Payload<'_>) -> Result<()> {
        let (program, args, bytes): (&str, Vec<String>, &[u8]) = match (self.tool, payload) {
            (LinuxTool::WlClipboard, Payload::Text(t)) => ("wl-copy", vec![], t.as_bytes()),
            (LinuxTool::WlClipboard, Payload::Image { bytes, mime }) => {
                ("wl-copy", vec!["--type".into(), mime.into()], bytes)
            }
            (LinuxTool::Xclip, Payload::Text(t)) => (
                "xclip",
                vec!["-selection".into(), "clipboard".into(), "-i".into()],
                t.as_bytes(),
            ),
            (LinuxTool::Xclip, Payload::Image { bytes, mime }) => (
                "xclip",
                vec![
                    "-selection".into(),
                    "clipboard".into(),
                    "-t".into(),
                    mime.into(),
                    "-i".into(),
                ],
                bytes,
            ),
            (LinuxTool::Xsel, Payload::Text(t)) => (
                "xsel",
                vec!["--clipboard".into(), "--input".into()],
                t.as_bytes(),
            ),
            (LinuxTool::Xsel, Payload::Image { mime, .. }) => {
                return Err(Error::UnsupportedContent {
                    mime: mime.to_string(),
                })
            }
        };

        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run(program, &arg_refs, Some(bytes))?;

        // Absorb our own write so the next poll does not replay it.
        if let Ok(Some(clip)) = self.read_current() {
            self.last_digest = Some(clip.digest());
        }
        Ok(())
    }
}

/// Prefer the helper matching the session type, then fall back to anything
/// installed: a Wayland session running XWayland can still use `xclip`.
fn detect_tool() -> Option<LinuxTool> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let candidates: [LinuxTool; 3] = if wayland {
        [LinuxTool::WlClipboard, LinuxTool::Xclip, LinuxTool::Xsel]
    } else {
        [LinuxTool::Xclip, LinuxTool::Xsel, LinuxTool::WlClipboard]
    };
    candidates.into_iter().find(|t| match t {
        LinuxTool::WlClipboard => binary_exists("wl-paste") && binary_exists("wl-copy"),
        LinuxTool::Xclip => binary_exists("xclip"),
        LinuxTool::Xsel => binary_exists("xsel"),
    })
}

fn binary_exists(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(name);
        std::fs::metadata(&candidate)
            .map(|m| m.is_file())
            .unwrap_or(false)
    })
}

/// Run a helper, returning stdout. Errors on spawn failure or a non-zero exit.
fn run(program: &str, args: &[&str], stdin_bytes: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(if stdin_bytes.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| Error::HelperSpawn {
            tool: program.to_string(),
            source,
        })?;

    if let Some(bytes) = stdin_bytes {
        let mut stdin = child.stdin.take().expect("stdin was piped");
        // A helper that exits early (broken pipe) is not our problem to report;
        // the exit status below is what matters.
        let _ = stdin.write_all(bytes);
        drop(stdin);
    }

    let out = child
        .wait_with_output()
        .map_err(|source| Error::HelperSpawn {
            tool: program.to_string(),
            source,
        })?;

    if !out.status.success() {
        return Err(Error::HelperFailed {
            tool: program.to_string(),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(out.stdout)
}

/// Like [`run`], but a failure just means "nothing there".
///
/// `wl-paste` exits non-zero on an empty clipboard, and `xclip` does the same
/// when asked for a target it does not have, so both are expected outcomes
/// rather than errors.
fn run_ok(program: &str, args: &[&str], stdin_bytes: Option<&[u8]>) -> Option<Vec<u8>> {
    match run(program, args, stdin_bytes) {
        Ok(out) => Some(out),
        Err(e) => {
            tracing::trace!(error = %e, program, "clipboard helper returned nothing");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_capabilities() {
        assert!(LinuxTool::WlClipboard.supports_images());
        assert!(LinuxTool::Xclip.supports_images());
        assert!(!LinuxTool::Xsel.supports_images());
    }

    #[test]
    fn binary_exists_finds_a_standard_tool() {
        assert!(binary_exists("sh"), "sh must be on PATH");
        assert!(!binary_exists("pasteport-definitely-not-a-real-binary"));
    }

    #[test]
    fn xsel_refuses_images_rather_than_writing_garbage() {
        let mut cb = LinuxClipboard::with_tool(LinuxTool::Xsel);
        let err = cb
            .write(Payload::Image {
                bytes: &[1, 2, 3],
                mime: "image/png",
            })
            .expect_err("xsel cannot carry images");
        assert!(matches!(err, Error::UnsupportedContent { .. }));
    }

    #[test]
    fn missing_helper_is_a_spawn_error() {
        let err = run("pasteport-definitely-not-a-real-binary", &[], None).unwrap_err();
        assert!(matches!(err, Error::HelperSpawn { .. }));
    }

    #[test]
    fn run_captures_stdout_and_pipes_stdin() {
        let out = run("cat", &[], Some(b"round trip")).unwrap();
        assert_eq!(out, b"round trip");
    }

    #[test]
    fn run_ok_swallows_failures() {
        assert!(run_ok("false", &[], None).is_none());
        assert!(run_ok("true", &[], None).is_some());
    }
}
