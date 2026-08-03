//! macOS backend built directly on `NSPasteboard`.
//!
//! Deliberately uses raw `msg_send!` rather than the generated `objc2-app-kit`
//! bindings. `NSPasteboard` is usable off the main thread, but the generated
//! bindings model it as main-thread-only, which would force the daemon's
//! watcher onto a run loop it does not otherwise need.

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::NSString;

use pasteport_core::NewClip;

use crate::{is_concealed, ClipboardBackend, Error, Payload, Result};

// `class!(NSPasteboard)` and `NSWorkspace` live in AppKit, which we are not
// otherwise linking against.
#[link(name = "AppKit", kind = "framework")]
extern "C" {}

const TYPE_UTF8: &str = "public.utf8-plain-text";
const TYPE_RTF: &str = "public.rtf";
const TYPE_PNG: &str = "public.png";
const TYPE_TIFF: &str = "public.tiff";
const TYPE_FILE_URL: &str = "public.file-url";

/// The macOS general pasteboard.
pub struct MacOsClipboard {
    /// `NSPasteboard.changeCount` as of the last poll. Comparing it is a single
    /// integer read, so idling costs almost nothing.
    last_change_count: isize,
}

impl std::fmt::Debug for MacOsClipboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MacOsClipboard")
            .field("last_change_count", &self.last_change_count)
            .finish()
    }
}

impl MacOsClipboard {
    /// Attach to the general pasteboard.
    ///
    /// Seeds change tracking with the current count, so whatever happens to be
    /// on the clipboard at startup is not captured as if the user just copied
    /// it. Call [`read_now`](ClipboardBackend::read_now) to pick it up on
    /// purpose.
    pub fn new() -> Result<Self> {
        let current = unsafe { change_count()? };
        Ok(MacOsClipboard {
            last_change_count: current,
        })
    }
}

impl ClipboardBackend for MacOsClipboard {
    fn name(&self) -> &'static str {
        "macos-nspasteboard"
    }

    fn poll(&mut self) -> Result<Option<NewClip>> {
        let current = unsafe { change_count()? };
        if current == self.last_change_count {
            return Ok(None);
        }
        self.last_change_count = current;
        unsafe { read_pasteboard() }
    }

    fn read_now(&mut self) -> Result<Option<NewClip>> {
        self.last_change_count = unsafe { change_count()? };
        unsafe { read_pasteboard() }
    }

    fn write(&mut self, payload: Payload<'_>) -> Result<()> {
        unsafe {
            let pb = pasteboard()?;
            let _: isize = msg_send![&*pb, clearContents];

            let ok: bool = match payload {
                Payload::Text(text) => {
                    let ns = NSString::from_str(text);
                    let ty = NSString::from_str(TYPE_UTF8);
                    msg_send![&*pb, setString: &*ns, forType: &*ty]
                }
                Payload::Image { bytes, mime } => {
                    let ty = NSString::from_str(mime_to_pasteboard_type(mime));
                    let data: Retained<AnyObject> = msg_send![
                        class!(NSData),
                        dataWithBytes: bytes.as_ptr() as *const c_void,
                        length: bytes.len(),
                    ];
                    msg_send![&*pb, setData: &*data, forType: &*ty]
                }
            };

            // Our own write bumps changeCount; absorb it so the next poll does
            // not report it back as something the user copied.
            self.last_change_count = change_count()?;

            if ok {
                Ok(())
            } else {
                Err(Error::ClipboardUnavailable)
            }
        }
    }
}

fn mime_to_pasteboard_type(mime: &str) -> &str {
    match mime {
        "image/tiff" => TYPE_TIFF,
        _ => TYPE_PNG,
    }
}

unsafe fn pasteboard() -> Result<Retained<AnyObject>> {
    let pb: Option<Retained<AnyObject>> = msg_send![class!(NSPasteboard), generalPasteboard];
    pb.ok_or(Error::ClipboardUnavailable)
}

unsafe fn change_count() -> Result<isize> {
    let pb = pasteboard()?;
    Ok(msg_send![&*pb, changeCount])
}

/// The UTIs currently on the pasteboard.
unsafe fn pasteboard_types(pb: &AnyObject) -> Vec<String> {
    let types: Option<Retained<AnyObject>> = msg_send![pb, types];
    let Some(types) = types else {
        return Vec::new();
    };

    let count: usize = msg_send![&*types, count];
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let item: Option<Retained<NSString>> = msg_send![&*types, objectAtIndex: i];
        if let Some(s) = item {
            out.push(s.to_string());
        }
    }
    out
}

unsafe fn string_for_type(pb: &AnyObject, ty: &str) -> Option<String> {
    let ns_ty = NSString::from_str(ty);
    let value: Option<Retained<NSString>> = msg_send![pb, stringForType: &*ns_ty];
    value.map(|s| s.to_string())
}

unsafe fn data_for_type(pb: &AnyObject, ty: &str) -> Option<Vec<u8>> {
    let ns_ty = NSString::from_str(ty);
    let data: Option<Retained<AnyObject>> = msg_send![pb, dataForType: &*ns_ty];
    let data = data?;

    let len: usize = msg_send![&*data, length];
    if len == 0 {
        return None;
    }
    let ptr: *const u8 = msg_send![&*data, bytes];
    if ptr.is_null() {
        return None;
    }
    Some(std::slice::from_raw_parts(ptr, len).to_vec())
}

unsafe fn read_pasteboard() -> Result<Option<NewClip>> {
    let pb = pasteboard()?;
    let types = pasteboard_types(&pb);

    let (app, bundle_id) = frontmost_app();

    // Check the do-not-archive markers before touching any payload, so a
    // password never enters our address space in the first place.
    if is_concealed(&types) {
        tracing::debug!("pasteboard item marked concealed; not reading contents");
        let mut clip = NewClip::text("");
        clip.text = None;
        clip.mime = "application/x-concealed".to_string();
        return Ok(Some(clip.concealed(true).with_source(app, bundle_id)));
    }

    let has = |t: &str| types.iter().any(|x| x == t);

    // File references first: they also carry a text representation, and the
    // path is the more useful thing to remember.
    if has(TYPE_FILE_URL) {
        if let Some(url) = string_for_type(&pb, TYPE_FILE_URL) {
            let mut clip = NewClip::text(url);
            clip.kind = pasteport_core::ClipKind::File;
            clip.mime = "text/uri-list".to_string();
            return Ok(Some(clip.with_source(app, bundle_id)));
        }
    }

    if has(TYPE_PNG) || has(TYPE_TIFF) {
        let (ty, mime) = if has(TYPE_PNG) {
            (TYPE_PNG, "image/png")
        } else {
            (TYPE_TIFF, "image/tiff")
        };
        if let Some(bytes) = data_for_type(&pb, ty) {
            return Ok(Some(
                NewClip::image(bytes, mime).with_source(app, bundle_id),
            ));
        }
    }

    if let Some(text) = string_for_type(&pb, TYPE_UTF8) {
        let mut clip = NewClip::text(text);
        // Keep rich text flagged so the UI can offer "paste with formatting",
        // while still storing the plain text for search.
        if has(TYPE_RTF) {
            clip.kind = pasteport_core::ClipKind::RichText;
            clip.mime = "text/rtf".to_string();
        }
        return Ok(Some(clip.with_source(app, bundle_id)));
    }

    if types.is_empty() {
        return Ok(None);
    }
    tracing::debug!(
        ?types,
        "pasteboard holds no representation Pasteport understands"
    );
    Ok(None)
}

/// Name and bundle id of the app in the foreground, used for the ignore list
/// and the "copied from" label. Best effort: `None` is fine.
fn frontmost_app() -> (Option<String>, Option<String>) {
    unsafe {
        let ws: Option<Retained<AnyObject>> = msg_send![class!(NSWorkspace), sharedWorkspace];
        let Some(ws) = ws else { return (None, None) };
        let app: Option<Retained<AnyObject>> = msg_send![&*ws, frontmostApplication];
        let Some(app) = app else { return (None, None) };

        let name: Option<Retained<NSString>> = msg_send![&*app, localizedName];
        let bundle: Option<Retained<NSString>> = msg_send![&*app, bundleIdentifier];
        (name.map(|s| s.to_string()), bundle.map(|s| s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_mime_to_pasteboard_type() {
        assert_eq!(mime_to_pasteboard_type("image/tiff"), TYPE_TIFF);
        assert_eq!(mime_to_pasteboard_type("image/png"), TYPE_PNG);
        assert_eq!(mime_to_pasteboard_type("image/gibberish"), TYPE_PNG);
    }

    /// Exercises the real pasteboard. Ignored by default so `cargo test` in CI
    /// never fights a developer's clipboard; run with
    /// `cargo test -- --ignored --test-threads=1`.
    #[test]
    #[ignore = "touches the user's real clipboard"]
    fn round_trips_text_through_the_real_pasteboard() {
        let mut cb = MacOsClipboard::new().unwrap();
        cb.write(Payload::Text("pasteport round trip")).unwrap();

        // Our own write must not surface as a user copy.
        assert!(cb.poll().unwrap().is_none());

        let clip = cb.read_now().unwrap().expect("something on the pasteboard");
        assert_eq!(clip.text.as_deref(), Some("pasteport round trip"));
    }
}
