use serde::{Deserialize, Serialize};

/// The kind of thing a clip holds. Drives icons, previews, and filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipKind {
    Text,
    RichText,
    Link,
    Color,
    Image,
    File,
}

impl ClipKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ClipKind::Text => "text",
            ClipKind::RichText => "rich_text",
            ClipKind::Link => "link",
            ClipKind::Color => "color",
            ClipKind::Image => "image",
            ClipKind::File => "file",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "text" => ClipKind::Text,
            "rich_text" => ClipKind::RichText,
            "link" => ClipKind::Link,
            "color" => ClipKind::Color,
            "image" => ClipKind::Image,
            "file" => ClipKind::File,
            _ => return None,
        })
    }
}

/// A clip as it arrives from a platform clipboard backend, before the store
/// assigns it an id or decides whether it is a duplicate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewClip {
    pub kind: ClipKind,
    pub mime: String,
    /// Searchable, human-readable representation. Always set for text-ish
    /// kinds; for images it may hold a filename or OCR text later on.
    pub text: Option<String>,
    /// Raw payload for binary kinds. Text kinds leave this empty.
    pub bytes: Option<Vec<u8>>,
    pub source_app: Option<String>,
    pub source_bundle_id: Option<String>,
    /// Set when the OS marked the pasteboard item as transient or concealed
    /// (password fields, one-time codes). Concealed clips are never stored.
    pub concealed: bool,
}

impl NewClip {
    /// Build a text clip, inferring a more specific kind from the content.
    pub fn text(body: impl Into<String>) -> Self {
        let body = body.into();
        let kind = infer_text_kind(&body);
        NewClip {
            kind,
            mime: "text/plain".to_string(),
            text: Some(body),
            bytes: None,
            source_app: None,
            source_bundle_id: None,
            concealed: false,
        }
    }

    /// Build an image clip from raw encoded image bytes.
    pub fn image(bytes: Vec<u8>, mime: impl Into<String>) -> Self {
        NewClip {
            kind: ClipKind::Image,
            mime: mime.into(),
            text: None,
            bytes: Some(bytes),
            source_app: None,
            source_bundle_id: None,
            concealed: false,
        }
    }

    pub fn with_source(mut self, app: Option<String>, bundle_id: Option<String>) -> Self {
        self.source_app = app;
        self.source_bundle_id = bundle_id;
        self
    }

    pub fn concealed(mut self, concealed: bool) -> Self {
        self.concealed = concealed;
        self
    }

    /// Content-addressed digest used for deduplication. Deliberately covers
    /// only the payload and mime, not the source app or timestamps, so the
    /// same text copied twice from different apps collapses into one row.
    pub fn digest(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.mime.as_bytes());
        hasher.update(&[0]);
        hasher.update(self.kind.as_str().as_bytes());
        hasher.update(&[0]);
        if let Some(text) = &self.text {
            hasher.update(text.as_bytes());
        }
        hasher.update(&[0]);
        if let Some(bytes) = &self.bytes {
            hasher.update(bytes);
        }
        hasher.finalize().to_hex().to_string()
    }

    pub fn byte_len(&self) -> usize {
        self.text.as_ref().map_or(0, |t| t.len()) + self.bytes.as_ref().map_or(0, |b| b.len())
    }

    /// True when there is nothing worth storing: no payload, or text that is
    /// entirely whitespace.
    pub fn is_empty(&self) -> bool {
        let text_empty = self.text.as_ref().is_none_or(|t| t.trim().is_empty());
        let bytes_empty = self.bytes.as_ref().is_none_or(|b| b.is_empty());
        text_empty && bytes_empty
    }
}

/// A stored clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: i64,
    pub kind: ClipKind,
    pub mime: String,
    pub text: Option<String>,
    /// Only populated by [`crate::Store::clip_bytes`]; list queries leave it
    /// empty so that scrolling a long history never pulls image blobs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<Vec<u8>>,
    pub byte_len: i64,
    pub source_app: Option<String>,
    pub source_bundle_id: Option<String>,
    pub hash: String,
    pub created_at: i64,
    pub last_used_at: i64,
    pub use_count: i64,
    pub pinned: bool,
}

impl Clip {
    /// A single-line preview, collapsed and truncated for list rendering.
    pub fn preview(&self, max_chars: usize) -> String {
        let raw = match (&self.text, self.kind) {
            (Some(t), _) => t.split_whitespace().collect::<Vec<_>>().join(" "),
            (None, ClipKind::Image) => format!("Image ({} bytes)", self.byte_len),
            (None, _) => format!("{} ({} bytes)", self.kind.as_str(), self.byte_len),
        };
        truncate_chars(&raw, max_chars)
    }

    pub fn created_at_rfc3339(&self) -> String {
        format_unix(self.created_at)
    }

    pub fn last_used_at_rfc3339(&self) -> String {
        format_unix(self.last_used_at)
    }
}

fn format_unix(secs: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(secs)
        .ok()
        .and_then(|dt| {
            dt.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_else(|| secs.to_string())
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Promote plain text to a more specific kind when the shape is unambiguous.
fn infer_text_kind(body: &str) -> ClipKind {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return ClipKind::Text;
    }
    if is_url(trimmed) {
        return ClipKind::Link;
    }
    if is_color(trimmed) {
        return ClipKind::Color;
    }
    ClipKind::Text
}

fn is_url(s: &str) -> bool {
    if s.split_whitespace().count() != 1 {
        return false;
    }
    ["https://", "http://", "ftp://", "ssh://", "mailto:"]
        .iter()
        .any(|scheme| s.len() > scheme.len() && s.starts_with(scheme))
}

fn is_color(s: &str) -> bool {
    if let Some(hex) = s.strip_prefix('#') {
        return matches!(hex.len(), 3 | 4 | 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit());
    }
    let lower = s.to_ascii_lowercase();
    (lower.starts_with("rgb(")
        || lower.starts_with("rgba(")
        || lower.starts_with("hsl(")
        || lower.starts_with("hsla("))
        && lower.ends_with(')')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_links() {
        assert_eq!(NewClip::text("https://example.com/x").kind, ClipKind::Link);
        assert_eq!(NewClip::text("mailto:a@b.co").kind, ClipKind::Link);
        // Bare scheme with no host is not a link.
        assert_eq!(NewClip::text("https://").kind, ClipKind::Text);
        // Prose that merely mentions a URL stays text.
        assert_eq!(
            NewClip::text("see https://example.com ok").kind,
            ClipKind::Text
        );
    }

    #[test]
    fn infers_colors() {
        assert_eq!(NewClip::text("#ff00aa").kind, ClipKind::Color);
        assert_eq!(NewClip::text("#FFF").kind, ClipKind::Color);
        assert_eq!(NewClip::text("rgba(1,2,3,0.5)").kind, ClipKind::Color);
        assert_eq!(NewClip::text("#zzzzzz").kind, ClipKind::Text);
        assert_eq!(NewClip::text("#12345").kind, ClipKind::Text);
    }

    #[test]
    fn digest_ignores_provenance_but_not_payload() {
        let a = NewClip::text("hello").with_source(Some("Safari".into()), None);
        let b = NewClip::text("hello").with_source(Some("Terminal".into()), None);
        let c = NewClip::text("goodbye");
        assert_eq!(a.digest(), b.digest());
        assert_ne!(a.digest(), c.digest());
    }

    #[test]
    fn empty_detection_treats_whitespace_as_empty() {
        assert!(NewClip::text("   \n\t ").is_empty());
        assert!(!NewClip::text("x").is_empty());
        assert!(!NewClip::image(vec![1, 2, 3], "image/png").is_empty());
        assert!(NewClip::image(vec![], "image/png").is_empty());
    }

    #[test]
    fn preview_truncates_on_char_boundaries() {
        let clip = Clip {
            id: 1,
            kind: ClipKind::Text,
            mime: "text/plain".into(),
            text: Some("émoji ➜ ✨ and a long tail of text".into()),
            bytes: None,
            byte_len: 10,
            source_app: None,
            source_bundle_id: None,
            hash: "h".into(),
            created_at: 0,
            last_used_at: 0,
            use_count: 1,
            pinned: false,
        };
        let p = clip.preview(10);
        assert_eq!(p.chars().count(), 10);
        assert!(p.ends_with('…'));
    }

    #[test]
    fn preview_collapses_newlines() {
        let clip = Clip {
            id: 1,
            kind: ClipKind::Text,
            mime: "text/plain".into(),
            text: Some("line one\n\n   line two".into()),
            bytes: None,
            byte_len: 10,
            source_app: None,
            source_bundle_id: None,
            hash: "h".into(),
            created_at: 0,
            last_used_at: 0,
            use_count: 1,
            pinned: false,
        };
        assert_eq!(clip.preview(100), "line one line two");
    }
}
