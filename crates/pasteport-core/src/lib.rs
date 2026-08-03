//! Pasteport's platform-agnostic clipboard history engine.
//!
//! Everything that decides *what* gets remembered, how it is searched, and how
//! long it lives is here. Nothing in this crate touches a platform clipboard;
//! that is [`pasteport-clipboard`]'s job. The split is what lets the same
//! engine back a SwiftUI app on macOS and a GTK4 app on Linux.
//!
//! ```
//! use pasteport_core::{Config, NewClip, Store};
//!
//! let store = Store::open_in_memory()?;
//! let cfg = Config::default();
//! store.insert(&NewClip::text("hello"), &cfg)?;
//! assert_eq!(store.search("hello", 10)?.len(), 1);
//! # Ok::<(), pasteport_core::Error>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod clip;
pub mod config;
pub mod error;
pub mod paths;
pub mod store;

pub use clip::{Clip, ClipKind, NewClip};
pub use config::Config;
pub use error::{Error, Result};
pub use store::{InsertOutcome, Pinboard, SkipReason, Stats, Store};

/// The version of the engine, surfaced in `pasteport status` and the about box.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
