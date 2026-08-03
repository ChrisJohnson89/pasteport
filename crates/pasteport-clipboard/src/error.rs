/// Failures reading or writing a platform clipboard.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Pasteport has no clipboard backend for {0}")]
    UnsupportedPlatform(&'static str),

    /// Linux only: none of the supported helper tools are installed.
    #[error(
        "no clipboard helper found. Install wl-clipboard (Wayland) or xclip/xsel (X11):\n  \
         Debian/Ubuntu: sudo apt install wl-clipboard xclip\n  \
         Fedora:        sudo dnf install wl-clipboard xclip\n  \
         Arch:          sudo pacman -S wl-clipboard xclip"
    )]
    NoHelperTool,

    #[error("clipboard helper {tool} failed with status {status}: {stderr}")]
    HelperFailed {
        tool: String,
        status: String,
        stderr: String,
    },

    #[error("could not run clipboard helper {tool}: {source}")]
    HelperSpawn {
        tool: String,
        #[source]
        source: std::io::Error,
    },

    #[error("the system clipboard is unavailable")]
    ClipboardUnavailable,

    #[error("clipboard contained {mime}, which Pasteport cannot represent")]
    UnsupportedContent { mime: String },
}

pub type Result<T> = std::result::Result<T, Error>;
