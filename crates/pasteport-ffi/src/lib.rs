//! C ABI for the Pasteport engine.
//!
//! The SwiftUI app links this static library and talks to the daemon through it,
//! rather than reimplementing the socket protocol in Swift. The surface is
//! deliberately tiny: connect, send a JSON request, get a JSON response, free
//! the string. Everything richer is modelled in Swift on top of `Codable`,
//! where it belongs.
//!
//! ```text
//! pasteport_client_connect(socket) -> handle
//! pasteport_client_request(handle, json) -> json   // caller frees
//! pasteport_string_free(json)
//! pasteport_client_free(handle)
//! ```
//!
//! Every entry point catches panics and returns an error instead, because a
//! panic unwinding across the FFI boundary into Swift is undefined behaviour.

use std::ffi::{c_char, CStr, CString};
use std::path::PathBuf;

use pasteport_daemon::protocol::Response;
use pasteport_daemon::Client;

/// Opaque connection handle handed to Swift.
pub struct ClientHandle {
    client: Client,
}

/// Engine version as a NUL-terminated string. Statically allocated; do not free.
#[no_mangle]
pub extern "C" fn pasteport_version() -> *const c_char {
    // The trailing NUL is part of the literal, so this is a valid C string.
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char
}

/// The default control socket path, or NULL if it cannot be determined.
///
/// Caller must free with [`pasteport_string_free`].
#[no_mangle]
pub extern "C" fn pasteport_default_socket_path() -> *mut c_char {
    guard_ptr(|| {
        let path = pasteport_core::paths::socket_path().ok()?;
        into_c_string(path.to_string_lossy().into_owned())
    })
}

/// Connect to the daemon. Pass NULL to use the default socket path.
///
/// Returns NULL when the daemon is not reachable. The caller owns the handle and
/// must release it with [`pasteport_client_free`].
///
/// # Safety
///
/// `socket_path` must be NULL or a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn pasteport_client_connect(socket_path: *const c_char) -> *mut ClientHandle {
    let result = std::panic::catch_unwind(|| {
        let path = match unsafe { optional_str(socket_path) } {
            Some(s) => PathBuf::from(s),
            None => pasteport_core::paths::socket_path().ok()?,
        };
        let client = Client::connect(&path).ok()?;
        Some(Box::into_raw(Box::new(ClientHandle { client })))
    });
    match result {
        Ok(Some(ptr)) => ptr,
        _ => std::ptr::null_mut(),
    }
}

/// Send a JSON request line and return the JSON response.
///
/// Never returns NULL for a live handle: transport failures come back as a
/// protocol-shaped `{"result":"error",...}` object, so Swift has exactly one
/// response type to decode.
///
/// Caller must free the result with [`pasteport_string_free`].
///
/// # Safety
///
/// `handle` must be a pointer from [`pasteport_client_connect`] that has not yet
/// been freed, and `request_json` must be a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn pasteport_client_request(
    handle: *mut ClientHandle,
    request_json: *const c_char,
) -> *mut c_char {
    if handle.is_null() {
        return error_json("client handle is null");
    }
    let Some(request) = (unsafe { optional_str(request_json) }) else {
        return error_json("request JSON is null or not valid UTF-8");
    };

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let handle = unsafe { &mut *handle };
        let parsed = match serde_json::from_str(&request) {
            Ok(req) => req,
            Err(e) => return error_json(format!("could not parse request: {e}")),
        };
        match handle.client.request(&parsed) {
            Ok(response) => match serde_json::to_string(&response) {
                Ok(json) => into_c_string(json).unwrap_or_else(|| error_json("response had a NUL")),
                Err(e) => error_json(format!("could not encode response: {e}")),
            },
            Err(e) => error_json(e),
        }
    }));

    result.unwrap_or_else(|_| error_json("the Pasteport engine panicked"))
}

/// Release a client handle.
///
/// # Safety
///
/// `handle` must come from [`pasteport_client_connect`] and must not be used
/// afterwards. Passing NULL is allowed and does nothing.
#[no_mangle]
pub unsafe extern "C" fn pasteport_client_free(handle: *mut ClientHandle) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle) });
    }
}

/// Free a string returned by this library.
///
/// # Safety
///
/// `s` must be a pointer this library returned and not yet freed. NULL is
/// allowed and does nothing.
#[no_mangle]
pub unsafe extern "C" fn pasteport_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
}

// ---- helpers -----------------------------------------------------------

/// Read an optional C string. Returns `None` for NULL or invalid UTF-8.
unsafe fn optional_str(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .ok()
        .map(|s| s.to_string())
}

fn into_c_string(s: String) -> Option<*mut c_char> {
    // Clipboard text can legitimately contain a NUL, which a C string cannot
    // carry. It only reaches here inside JSON, where serde escapes it, so this
    // is a belt-and-braces check rather than an expected path.
    CString::new(s).ok().map(|c| c.into_raw())
}

/// Build a protocol-shaped error response, so callers have one thing to decode.
fn error_json(message: impl std::fmt::Display) -> *mut c_char {
    let response = Response::error(message);
    let json = serde_json::to_string(&response)
        .unwrap_or_else(|_| r#"{"result":"error","message":"unknown error"}"#.to_string());
    match CString::new(json) {
        Ok(c) => c.into_raw(),
        // Last resort: a literal that cannot fail to build.
        Err(_) => CString::new(r#"{"result":"error","message":"unknown error"}"#)
            .expect("literal has no NUL")
            .into_raw(),
    }
}

/// Run a fallible closure, mapping both `None` and a panic to NULL.
fn guard_ptr<F>(f: F) -> *mut c_char
where
    F: FnOnce() -> Option<*mut c_char> + std::panic::UnwindSafe,
{
    match std::panic::catch_unwind(f) {
        Ok(Some(ptr)) => ptr,
        _ => std::ptr::null_mut(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> CString {
        CString::new(s).unwrap()
    }

    /// Read a returned string and free it.
    unsafe fn take(ptr: *mut c_char) -> String {
        assert!(!ptr.is_null());
        let s = CStr::from_ptr(ptr).to_str().unwrap().to_string();
        pasteport_string_free(ptr);
        s
    }

    #[test]
    fn version_is_a_valid_c_string() {
        let ptr = pasteport_version();
        let s = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap();
        assert_eq!(s, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn default_socket_path_is_returned_and_freeable() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("PASTEPORT_DATA_DIR", dir.path());

        let s = unsafe { take(pasteport_default_socket_path()) };
        assert!(s.ends_with("daemon.sock"), "got {s}");
    }

    #[test]
    fn connecting_to_nothing_returns_null_rather_than_crashing() {
        let path = c("/tmp/pasteport-definitely-not-a-socket-12345");
        let handle = unsafe { pasteport_client_connect(path.as_ptr()) };
        assert!(handle.is_null());
        // Freeing a null handle is a no-op, not a crash.
        unsafe { pasteport_client_free(handle) };
    }

    #[test]
    fn a_null_handle_yields_a_protocol_error() {
        let req = c(r#"{"op":"ping"}"#);
        let json = unsafe { take(pasteport_client_request(std::ptr::null_mut(), req.as_ptr())) };
        let response: Response = serde_json::from_str(&json).unwrap();
        assert!(response.is_error(), "got {json}");
    }

    #[test]
    fn errors_come_back_as_decodable_protocol_responses() {
        let json = unsafe { take(error_json("something went wrong")) };
        let response: Response = serde_json::from_str(&json).unwrap();
        match response {
            Response::Error { message } => assert_eq!(message, "something went wrong"),
            other => panic!("expected an error response, got {other:?}"),
        }
    }

    #[test]
    fn freeing_a_null_string_is_a_no_op() {
        unsafe { pasteport_string_free(std::ptr::null_mut()) };
    }

    #[test]
    fn strings_with_interior_nul_are_rejected_not_truncated() {
        assert!(into_c_string("bad\0string".to_string()).is_none());
    }
}
