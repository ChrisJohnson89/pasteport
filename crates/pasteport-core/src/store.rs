use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::clip::{Clip, ClipKind, NewClip};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::paths;

const SCHEMA_VERSION: i32 = 1;

/// Columns for list queries. Deliberately omits `bytes` so that paging through
/// history never loads image payloads into memory.
const CLIP_COLS: &str = "id, kind, mime, text, byte_len, source_app, source_bundle_id, \
                         hash, created_at, last_used_at, use_count, pinned";

/// What happened to a clip offered to the store.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum InsertOutcome {
    /// A new row was created.
    Stored(Clip),
    /// Identical content already existed; its recency was bumped instead.
    Deduped(Clip),
    /// Deliberately not stored.
    Skipped(SkipReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    Empty,
    /// The OS marked the item concealed or transient (password fields).
    Concealed,
    /// Source app is on the ignore list.
    IgnoredSource,
    TooLarge,
    ImagesDisabled,
}

impl SkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            SkipReason::Empty => "empty",
            SkipReason::Concealed => "concealed",
            SkipReason::IgnoredSource => "ignored source",
            SkipReason::TooLarge => "too large",
            SkipReason::ImagesDisabled => "image capture disabled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pinboard {
    pub id: i64,
    pub name: String,
    pub position: i64,
    pub clip_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stats {
    pub total_clips: i64,
    pub pinned_clips: i64,
    pub pinboards: i64,
    pub total_bytes: i64,
    pub oldest_created_at: Option<i64>,
    pub full_text_search: bool,
}

/// The clipboard history database.
///
/// Single-connection and not internally synchronized; the daemon owns one and
/// serializes access. Callers that need concurrency should open their own.
pub struct Store {
    conn: Connection,
    fts: bool,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the connection: its Debug includes the database path, and
        // these get logged.
        f.debug_struct("Store")
            .field("full_text_search", &self.fts)
            .finish_non_exhaustive()
    }
}

impl Store {
    /// Open (creating if needed) the database at the standard location.
    pub fn open_default() -> Result<Self> {
        paths::ensure_data_dir()?;
        Self::open(&paths::database_path()?)
    }

    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        let conn = Connection::open(path)?;
        let store = Self::init(conn)?;
        // Owner-only, plus the WAL sidecars SQLite creates alongside it.
        paths::restrict_to_owner(path)?;
        for suffix in ["-wal", "-shm"] {
            let side = path.with_file_name(format!(
                "{}{suffix}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
            if side.exists() {
                paths::restrict_to_owner(&side)?;
            }
        }
        store.vacuum_if_needed()?;
        Ok(store)
    }

    /// Ephemeral store, used by tests and by `--private` sessions.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Keep temp material in memory: spilling clipboard contents to disk
        // outside our own 0600 files would leak history.
        conn.pragma_update(None, "temp_store", "MEMORY")?;

        let mut store = Store { conn, fts: false };
        store.migrate()?;
        store.fts = store.try_enable_fts()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i32 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        if version >= SCHEMA_VERSION {
            return Ok(());
        }
        if version == 0 {
            self.conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS clips (
                    id               INTEGER PRIMARY KEY AUTOINCREMENT,
                    kind             TEXT    NOT NULL,
                    mime             TEXT    NOT NULL,
                    text             TEXT,
                    bytes            BLOB,
                    byte_len         INTEGER NOT NULL DEFAULT 0,
                    source_app       TEXT,
                    source_bundle_id TEXT,
                    hash             TEXT    NOT NULL,
                    created_at       INTEGER NOT NULL,
                    last_used_at     INTEGER NOT NULL,
                    use_count        INTEGER NOT NULL DEFAULT 1,
                    pinned           INTEGER NOT NULL DEFAULT 0,
                    -- Monotonic use counter, bumped on insert and on every use.
                    -- Ordering keys off this rather than off last_used_at:
                    -- timestamps have one-second resolution, so two clips used
                    -- within the same second would order arbitrarily and
                    -- "paste from history moves it to the top" would visibly
                    -- fail. It is also immune to the system clock changing.
                    seq              INTEGER NOT NULL DEFAULT 0
                );
                CREATE UNIQUE INDEX IF NOT EXISTS clips_hash_idx ON clips(hash);
                CREATE INDEX IF NOT EXISTS clips_recent_idx ON clips(seq DESC);
                CREATE INDEX IF NOT EXISTS clips_kind_idx ON clips(kind, seq DESC);
                CREATE INDEX IF NOT EXISTS clips_pinned_idx ON clips(pinned, seq DESC);

                CREATE TABLE IF NOT EXISTS pinboards (
                    id         INTEGER PRIMARY KEY AUTOINCREMENT,
                    name       TEXT    NOT NULL UNIQUE,
                    position   INTEGER NOT NULL DEFAULT 0,
                    created_at INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS pinboard_clips (
                    pinboard_id INTEGER NOT NULL REFERENCES pinboards(id) ON DELETE CASCADE,
                    clip_id     INTEGER NOT NULL REFERENCES clips(id)     ON DELETE CASCADE,
                    position    INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (pinboard_id, clip_id)
                );
                CREATE INDEX IF NOT EXISTS pinboard_clips_clip_idx ON pinboard_clips(clip_id);
                "#,
            )?;
        }
        self.conn
            .pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(())
    }

    /// FTS5 is a compile-time option in SQLite, so we probe for it rather than
    /// assume it. Without it, search falls back to a `LIKE` scan, which is
    /// slower but correct.
    fn try_enable_fts(&self) -> Result<bool> {
        let probe = self
            .conn
            .execute_batch("CREATE VIRTUAL TABLE IF NOT EXISTS temp.pp_fts_probe USING fts5(x);");
        if probe.is_err() {
            tracing::info!("SQLite built without FTS5; falling back to LIKE search");
            return Ok(false);
        }
        let _ = self
            .conn
            .execute_batch("DROP TABLE IF EXISTS temp.pp_fts_probe;");

        let exists: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='clips_fts'",
                [],
                |r| r.get(0),
            )
            .optional()?;

        if exists.is_none() {
            self.conn.execute_batch(
                r#"
                CREATE VIRTUAL TABLE clips_fts USING fts5(
                    text,
                    content='clips',
                    content_rowid='id',
                    tokenize='unicode61 remove_diacritics 2'
                );
                CREATE TRIGGER clips_fts_ai AFTER INSERT ON clips BEGIN
                    INSERT INTO clips_fts(rowid, text) VALUES (new.id, new.text);
                END;
                CREATE TRIGGER clips_fts_ad AFTER DELETE ON clips BEGIN
                    INSERT INTO clips_fts(clips_fts, rowid, text)
                    VALUES ('delete', old.id, old.text);
                END;
                CREATE TRIGGER clips_fts_au AFTER UPDATE ON clips BEGIN
                    INSERT INTO clips_fts(clips_fts, rowid, text)
                    VALUES ('delete', old.id, old.text);
                    INSERT INTO clips_fts(rowid, text) VALUES (new.id, new.text);
                END;
                INSERT INTO clips_fts(clips_fts) VALUES ('rebuild');
                "#,
            )?;
        }
        Ok(true)
    }

    /// Reclaim space once deletions have left the file mostly empty.
    fn vacuum_if_needed(&self) -> Result<()> {
        let free: i64 = self
            .conn
            .query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
        let total: i64 = self.conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        if total > 1024 && free * 4 > total {
            tracing::debug!(free, total, "vacuuming history database");
            self.conn.execute_batch("VACUUM")?;
        }
        Ok(())
    }

    pub fn has_full_text_search(&self) -> bool {
        self.fts
    }

    // ---- writes ---------------------------------------------------------

    /// Offer a clip to the store, applying every policy in `cfg`.
    pub fn insert(&self, clip: &NewClip, cfg: &Config) -> Result<InsertOutcome> {
        self.insert_at(clip, cfg, now())
    }

    pub(crate) fn insert_at(
        &self,
        clip: &NewClip,
        cfg: &Config,
        now: i64,
    ) -> Result<InsertOutcome> {
        use InsertOutcome::Skipped;

        if clip.concealed {
            return Ok(Skipped(SkipReason::Concealed));
        }
        if clip.is_empty() {
            return Ok(Skipped(SkipReason::Empty));
        }
        if !cfg.capture_images && clip.kind == ClipKind::Image {
            return Ok(Skipped(SkipReason::ImagesDisabled));
        }
        if cfg.is_ignored_source(clip.source_app.as_deref(), clip.source_bundle_id.as_deref()) {
            return Ok(Skipped(SkipReason::IgnoredSource));
        }
        let len = clip.byte_len();
        if len > cfg.max_clip_bytes {
            return Ok(Skipped(SkipReason::TooLarge));
        }

        let hash = clip.digest();
        if let Some(existing) = self.find_by_hash(&hash)? {
            self.conn.execute(
                "UPDATE clips SET last_used_at = ?1, use_count = use_count + 1, \
                 seq = (SELECT COALESCE(MAX(seq), 0) + 1 FROM clips) WHERE id = ?2",
                params![now, existing.id],
            )?;
            let mut bumped = existing;
            bumped.last_used_at = now;
            bumped.use_count += 1;
            return Ok(InsertOutcome::Deduped(bumped));
        }

        self.conn.execute(
            "INSERT INTO clips (kind, mime, text, bytes, byte_len, source_app, \
             source_bundle_id, hash, created_at, last_used_at, use_count, pinned, seq) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, 1, 0, \
                     (SELECT COALESCE(MAX(seq), 0) + 1 FROM clips))",
            params![
                clip.kind.as_str(),
                clip.mime,
                clip.text,
                clip.bytes,
                len as i64,
                clip.source_app,
                clip.source_bundle_id,
                hash,
                now,
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        let stored = self.get(id)?;
        Ok(InsertOutcome::Stored(stored))
    }

    /// Record that a clip was pasted: bumps it to the top of the history.
    pub fn touch(&self, id: i64) -> Result<Clip> {
        let changed = self.conn.execute(
            "UPDATE clips SET last_used_at = ?1, use_count = use_count + 1, \
             seq = (SELECT COALESCE(MAX(seq), 0) + 1 FROM clips) WHERE id = ?2",
            params![now(), id],
        )?;
        if changed == 0 {
            return Err(Error::ClipNotFound(id));
        }
        self.get(id)
    }

    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<Clip> {
        let changed = self.conn.execute(
            "UPDATE clips SET pinned = ?1 WHERE id = ?2",
            params![pinned as i64, id],
        )?;
        if changed == 0 {
            return Err(Error::ClipNotFound(id));
        }
        self.get(id)
    }

    pub fn delete(&self, id: i64) -> Result<()> {
        let changed = self
            .conn
            .execute("DELETE FROM clips WHERE id = ?1", params![id])?;
        if changed == 0 {
            return Err(Error::ClipNotFound(id));
        }
        Ok(())
    }

    /// Wipe history. Pinned clips and pinboard members survive unless
    /// `include_pinned` is set.
    pub fn clear(&self, include_pinned: bool) -> Result<usize> {
        let n = if include_pinned {
            self.conn.execute("DELETE FROM clips", [])?
        } else {
            self.conn.execute(
                "DELETE FROM clips WHERE pinned = 0 \
                 AND id NOT IN (SELECT clip_id FROM pinboard_clips)",
                [],
            )?
        };
        Ok(n)
    }

    /// Enforce the retention policy. Never touches pinned clips or clips that
    /// belong to a pinboard.
    pub fn prune(&self, cfg: &Config) -> Result<usize> {
        self.prune_at(cfg, now())
    }

    pub(crate) fn prune_at(&self, cfg: &Config, now: i64) -> Result<usize> {
        let mut removed = 0;

        if cfg.retention_days > 0 {
            let cutoff = now - (cfg.retention_days as i64) * 86_400;
            removed += self.conn.execute(
                "DELETE FROM clips WHERE pinned = 0 AND created_at < ?1 \
                 AND id NOT IN (SELECT clip_id FROM pinboard_clips)",
                params![cutoff],
            )?;
        }

        removed += self.conn.execute(
            "DELETE FROM clips WHERE id IN (
                 SELECT id FROM clips
                 WHERE pinned = 0 AND id NOT IN (SELECT clip_id FROM pinboard_clips)
                 ORDER BY seq DESC
                 LIMIT -1 OFFSET ?1
             )",
            params![cfg.max_items as i64],
        )?;

        Ok(removed)
    }

    // ---- reads ----------------------------------------------------------

    pub fn get(&self, id: i64) -> Result<Clip> {
        self.conn
            .query_row(
                &format!("SELECT {CLIP_COLS} FROM clips WHERE id = ?1"),
                params![id],
                row_to_clip,
            )
            .optional()?
            .ok_or(Error::ClipNotFound(id))
    }

    /// The payload of a binary clip, loaded on demand.
    pub fn clip_bytes(&self, id: i64) -> Result<Option<Vec<u8>>> {
        let bytes: Option<Option<Vec<u8>>> = self
            .conn
            .query_row("SELECT bytes FROM clips WHERE id = ?1", params![id], |r| {
                r.get(0)
            })
            .optional()?;
        bytes.ok_or(Error::ClipNotFound(id))
    }

    fn find_by_hash(&self, hash: &str) -> Result<Option<Clip>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {CLIP_COLS} FROM clips WHERE hash = ?1"),
                params![hash],
                row_to_clip,
            )
            .optional()?)
    }

    /// Most recently used clips first, pinned ones floated to the top.
    pub fn recent(&self, limit: usize, offset: usize) -> Result<Vec<Clip>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CLIP_COLS} FROM clips \
             ORDER BY pinned DESC, seq DESC LIMIT ?1 OFFSET ?2"
        ))?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], row_to_clip)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn by_kind(&self, kind: ClipKind, limit: usize) -> Result<Vec<Clip>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CLIP_COLS} FROM clips WHERE kind = ?1 \
             ORDER BY pinned DESC, seq DESC LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![kind.as_str(), limit as i64], row_to_clip)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn pinned(&self, limit: usize) -> Result<Vec<Clip>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CLIP_COLS} FROM clips WHERE pinned = 1 \
             ORDER BY seq DESC LIMIT ?1"
        ))?;
        let rows = stmt.query_map(params![limit as i64], row_to_clip)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Search clip text. Uses FTS5 when available, otherwise a `LIKE` scan.
    ///
    /// The query is treated as literal words, never as FTS syntax, so pasting
    /// something like `a AND "b*` searches for those characters instead of
    /// erroring out.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Clip>> {
        let tokens = tokenize(query);
        if tokens.is_empty() {
            return self.recent(limit, 0);
        }
        if self.fts {
            match self.search_fts(&tokens, limit) {
                Ok(hits) => return Ok(hits),
                // A malformed MATCH expression should degrade, not fail.
                Err(e) => tracing::warn!(error = %e, "FTS search failed; falling back to LIKE"),
            }
        }
        self.search_like(query, limit)
    }

    fn search_fts(&self, tokens: &[String], limit: usize) -> Result<Vec<Clip>> {
        // Each token is quoted (so FTS operators inside it are inert) and the
        // last one gets a prefix wildcard for as-you-type matching.
        let last = tokens.len() - 1;
        let expr = tokens
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let quoted = t.replace('"', "\"\"");
                if i == last {
                    format!("\"{quoted}\"*")
                } else {
                    format!("\"{quoted}\"")
                }
            })
            .collect::<Vec<_>>()
            .join(" AND ");

        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM clips c JOIN clips_fts f ON f.rowid = c.id \
             WHERE clips_fts MATCH ?1 \
             ORDER BY c.pinned DESC, bm25(clips_fts), c.seq DESC LIMIT ?2",
            CLIP_COLS
                .split(", ")
                .map(|c| format!("c.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        ))?;
        let rows = stmt.query_map(params![expr, limit as i64], row_to_clip)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn search_like(&self, query: &str, limit: usize) -> Result<Vec<Clip>> {
        let pattern = format!("%{}%", escape_like(query));
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CLIP_COLS} FROM clips WHERE text LIKE ?1 ESCAPE '\\' \
             ORDER BY pinned DESC, seq DESC LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![pattern, limit as i64], row_to_clip)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn stats(&self) -> Result<Stats> {
        let (total_clips, pinned_clips, total_bytes, oldest_created_at) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(pinned), 0), COALESCE(SUM(byte_len), 0), MIN(created_at) \
             FROM clips",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        let pinboards = self
            .conn
            .query_row("SELECT COUNT(*) FROM pinboards", [], |r| r.get(0))?;
        Ok(Stats {
            total_clips,
            pinned_clips,
            pinboards,
            total_bytes,
            oldest_created_at,
            full_text_search: self.fts,
        })
    }

    // ---- pinboards ------------------------------------------------------

    pub fn create_pinboard(&self, name: &str) -> Result<Pinboard> {
        self.conn.execute(
            "INSERT INTO pinboards (name, position, created_at) \
             VALUES (?1, (SELECT COALESCE(MAX(position), -1) + 1 FROM pinboards), ?2) \
             ON CONFLICT(name) DO NOTHING",
            params![name, now()],
        )?;
        self.pinboard_by_name(name)
    }

    pub fn pinboard_by_name(&self, name: &str) -> Result<Pinboard> {
        self.conn
            .query_row(
                "SELECT p.id, p.name, p.position, \
                 (SELECT COUNT(*) FROM pinboard_clips pc WHERE pc.pinboard_id = p.id) \
                 FROM pinboards p WHERE p.name = ?1",
                params![name],
                |r| {
                    Ok(Pinboard {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        position: r.get(2)?,
                        clip_count: r.get(3)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| Error::PinboardNotFound(name.to_string()))
    }

    pub fn list_pinboards(&self) -> Result<Vec<Pinboard>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.name, p.position, \
             (SELECT COUNT(*) FROM pinboard_clips pc WHERE pc.pinboard_id = p.id) \
             FROM pinboards p ORDER BY p.position, p.id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Pinboard {
                id: r.get(0)?,
                name: r.get(1)?,
                position: r.get(2)?,
                clip_count: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn delete_pinboard(&self, name: &str) -> Result<()> {
        let changed = self
            .conn
            .execute("DELETE FROM pinboards WHERE name = ?1", params![name])?;
        if changed == 0 {
            return Err(Error::PinboardNotFound(name.to_string()));
        }
        Ok(())
    }

    pub fn add_to_pinboard(&self, name: &str, clip_id: i64) -> Result<()> {
        let board = self.pinboard_by_name(name)?;
        // Fail loudly on a missing clip rather than letting the foreign key
        // produce an opaque constraint error.
        self.get(clip_id)?;
        self.conn.execute(
            "INSERT INTO pinboard_clips (pinboard_id, clip_id, position) \
             VALUES (?1, ?2, (SELECT COALESCE(MAX(position), -1) + 1 FROM pinboard_clips \
                              WHERE pinboard_id = ?1)) \
             ON CONFLICT(pinboard_id, clip_id) DO NOTHING",
            params![board.id, clip_id],
        )?;
        Ok(())
    }

    pub fn remove_from_pinboard(&self, name: &str, clip_id: i64) -> Result<()> {
        let board = self.pinboard_by_name(name)?;
        self.conn.execute(
            "DELETE FROM pinboard_clips WHERE pinboard_id = ?1 AND clip_id = ?2",
            params![board.id, clip_id],
        )?;
        Ok(())
    }

    pub fn pinboard_clips(&self, name: &str, limit: usize) -> Result<Vec<Clip>> {
        let board = self.pinboard_by_name(name)?;
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM clips c JOIN pinboard_clips pc ON pc.clip_id = c.id \
             WHERE pc.pinboard_id = ?1 ORDER BY pc.position, pc.clip_id LIMIT ?2",
            CLIP_COLS
                .split(", ")
                .map(|c| format!("c.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        ))?;
        let rows = stmt.query_map(params![board.id, limit as i64], row_to_clip)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

fn row_to_clip(row: &rusqlite::Row<'_>) -> rusqlite::Result<Clip> {
    let kind: String = row.get(1)?;
    Ok(Clip {
        id: row.get(0)?,
        kind: ClipKind::parse(&kind).unwrap_or(ClipKind::Text),
        mime: row.get(2)?,
        text: row.get(3)?,
        bytes: None,
        byte_len: row.get(4)?,
        source_app: row.get(5)?,
        source_bundle_id: row.get(6)?,
        hash: row.get(7)?,
        created_at: row.get(8)?,
        last_used_at: row.get(9)?,
        use_count: row.get(10)?,
        pinned: row.get::<_, i64>(11)? != 0,
    })
}

pub(crate) fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// Split a search query into literal word tokens, discarding punctuation that
/// FTS5 would otherwise interpret as syntax.
fn tokenize(query: &str) -> Vec<String> {
    query
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    fn stored(outcome: InsertOutcome) -> Clip {
        match outcome {
            InsertOutcome::Stored(c) | InsertOutcome::Deduped(c) => c,
            InsertOutcome::Skipped(r) => panic!("unexpectedly skipped: {}", r.as_str()),
        }
    }

    #[test]
    fn stores_and_reads_back() {
        let s = store();
        let cfg = Config::default();
        let clip = stored(s.insert(&NewClip::text("hello world"), &cfg).unwrap());
        assert_eq!(clip.text.as_deref(), Some("hello world"));
        assert_eq!(clip.use_count, 1);
        assert!(!clip.pinned);
        assert_eq!(s.recent(10, 0).unwrap().len(), 1);
    }

    #[test]
    fn identical_content_dedupes_and_bumps() {
        let s = store();
        let cfg = Config::default();
        s.insert_at(&NewClip::text("same"), &cfg, 1_000).unwrap();
        let second = s.insert_at(&NewClip::text("same"), &cfg, 2_000).unwrap();

        assert!(matches!(second, InsertOutcome::Deduped(_)));
        let clip = stored(second);
        assert_eq!(clip.use_count, 2);
        assert_eq!(clip.last_used_at, 2_000);
        assert_eq!(
            clip.created_at, 1_000,
            "dedupe must preserve first-seen time"
        );
        assert_eq!(s.recent(10, 0).unwrap().len(), 1);
    }

    #[test]
    fn skips_concealed_empty_and_ignored() {
        let s = store();
        let cfg = Config::default();

        let concealed = s
            .insert(&NewClip::text("hunter2").concealed(true), &cfg)
            .unwrap();
        assert!(matches!(
            concealed,
            InsertOutcome::Skipped(SkipReason::Concealed)
        ));

        let empty = s.insert(&NewClip::text("  \n "), &cfg).unwrap();
        assert!(matches!(empty, InsertOutcome::Skipped(SkipReason::Empty)));

        let vault = NewClip::text("vault entry").with_source(
            Some("1Password".into()),
            Some("com.1password.1password".into()),
        );
        let ignored = s.insert(&vault, &cfg).unwrap();
        assert!(matches!(
            ignored,
            InsertOutcome::Skipped(SkipReason::IgnoredSource)
        ));

        assert!(
            s.recent(10, 0).unwrap().is_empty(),
            "nothing sensitive should be stored"
        );
    }

    #[test]
    fn enforces_size_limit() {
        let s = store();
        let cfg = Config {
            max_clip_bytes: 1024,
            ..Config::default()
        };
        let big = NewClip::text("x".repeat(2048));
        assert!(matches!(
            s.insert(&big, &cfg).unwrap(),
            InsertOutcome::Skipped(SkipReason::TooLarge)
        ));
    }

    #[test]
    fn honours_capture_images_toggle() {
        let s = store();
        let cfg = Config {
            capture_images: false,
            ..Config::default()
        };
        let img = NewClip::image(vec![1, 2, 3, 4], "image/png");
        assert!(matches!(
            s.insert(&img, &cfg).unwrap(),
            InsertOutcome::Skipped(SkipReason::ImagesDisabled)
        ));
    }

    #[test]
    fn image_bytes_round_trip_but_stay_out_of_list_queries() {
        let s = store();
        let cfg = Config::default();
        let payload = vec![0x89, 0x50, 0x4e, 0x47, 9, 9, 9];
        let clip = stored(
            s.insert(&NewClip::image(payload.clone(), "image/png"), &cfg)
                .unwrap(),
        );

        assert_eq!(s.clip_bytes(clip.id).unwrap(), Some(payload));
        assert!(s.recent(10, 0).unwrap()[0].bytes.is_none());
    }

    #[test]
    fn pinning_floats_to_top_and_survives_clear() {
        let s = store();
        let cfg = Config::default();
        let first = stored(s.insert_at(&NewClip::text("old"), &cfg, 1_000).unwrap());
        s.insert_at(&NewClip::text("new"), &cfg, 2_000).unwrap();

        s.set_pinned(first.id, true).unwrap();
        assert_eq!(s.recent(10, 0).unwrap()[0].id, first.id);

        let removed = s.clear(false).unwrap();
        assert_eq!(removed, 1);
        let left = s.recent(10, 0).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, first.id);

        s.clear(true).unwrap();
        assert!(s.recent(10, 0).unwrap().is_empty());
    }

    #[test]
    fn search_finds_by_word() {
        let s = store();
        let cfg = Config::default();
        s.insert(&NewClip::text("the quick brown fox"), &cfg)
            .unwrap();
        s.insert(&NewClip::text("unrelated content"), &cfg).unwrap();

        let hits = s.search("brown", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].text.as_deref().unwrap().contains("brown"));
    }

    #[test]
    fn search_supports_prefix_matching() {
        let s = store();
        let cfg = Config::default();
        s.insert(&NewClip::text("deployment checklist"), &cfg)
            .unwrap();
        assert_eq!(s.search("deploy", 10).unwrap().len(), 1);
    }

    #[test]
    fn search_treats_operators_as_literals() {
        let s = store();
        let cfg = Config::default();
        s.insert(&NewClip::text("alpha beta"), &cfg).unwrap();

        // None of these may error, whatever FTS would make of them.
        for q in [
            "alpha AND",
            "\"unbalanced",
            "alpha*(",
            "NEAR(",
            "^",
            "*",
            "-",
        ] {
            let _ = s.search(q, 10).unwrap();
        }
        assert_eq!(s.search("alpha beta", 10).unwrap().len(), 1);
    }

    #[test]
    fn search_with_blank_query_returns_recent() {
        let s = store();
        let cfg = Config::default();
        s.insert(&NewClip::text("something"), &cfg).unwrap();
        assert_eq!(s.search("   ", 10).unwrap().len(), 1);
    }

    #[test]
    fn like_fallback_matches_the_fts_path() {
        let s = store();
        let cfg = Config::default();
        s.insert(&NewClip::text("fallback path works"), &cfg)
            .unwrap();
        assert_eq!(s.search_like("fallback", 10).unwrap().len(), 1);
        // Wildcards in the query must not widen the match.
        assert!(s.search_like("%", 10).unwrap().is_empty());
    }

    #[test]
    fn search_index_stays_in_sync_after_delete() {
        let s = store();
        let cfg = Config::default();
        let clip = stored(
            s.insert(&NewClip::text("ephemeral secret note"), &cfg)
                .unwrap(),
        );
        assert_eq!(s.search("ephemeral", 10).unwrap().len(), 1);

        s.delete(clip.id).unwrap();
        assert!(
            s.search("ephemeral", 10).unwrap().is_empty(),
            "deleted text must not remain searchable"
        );
    }

    #[test]
    fn prune_respects_age_and_cap_but_never_pinned() {
        let s = store();
        let cfg = Config {
            retention_days: 7,
            max_items: 100,
            ..Config::default()
        };
        let now = 10_000_000;
        let old = now - 30 * 86_400;

        let ancient = stored(s.insert_at(&NewClip::text("ancient"), &cfg, old).unwrap());
        let ancient_pinned = stored(
            s.insert_at(&NewClip::text("ancient but pinned"), &cfg, old)
                .unwrap(),
        );
        s.set_pinned(ancient_pinned.id, true).unwrap();
        s.insert_at(&NewClip::text("fresh"), &cfg, now).unwrap();

        // Pinning bumps nothing, so backdate last_used_at too via prune_at's clock.
        let removed = s.prune_at(&cfg, now).unwrap();
        assert_eq!(removed, 1);
        assert!(matches!(s.get(ancient.id), Err(Error::ClipNotFound(_))));
        assert!(s.get(ancient_pinned.id).is_ok());
    }

    #[test]
    fn prune_enforces_max_items() {
        let s = store();
        let cfg = Config {
            retention_days: 0,
            max_items: 3,
            ..Config::default()
        };
        for i in 0..10 {
            s.insert_at(&NewClip::text(format!("clip {i}")), &cfg, 1_000 + i)
                .unwrap();
        }
        let removed = s.prune_at(&cfg, 2_000).unwrap();
        assert_eq!(removed, 7);

        let left = s.recent(10, 0).unwrap();
        assert_eq!(left.len(), 3);
        // The survivors are the most recent ones.
        assert_eq!(left[0].text.as_deref(), Some("clip 9"));
    }

    #[test]
    fn prune_keeps_pinboard_members() {
        let s = store();
        let cfg = Config {
            retention_days: 0,
            max_items: 1,
            ..Config::default()
        };
        let keep = stored(s.insert_at(&NewClip::text("keep me"), &cfg, 1_000).unwrap());
        s.create_pinboard("snippets").unwrap();
        s.add_to_pinboard("snippets", keep.id).unwrap();
        for i in 0..5 {
            s.insert_at(&NewClip::text(format!("filler {i}")), &cfg, 2_000 + i)
                .unwrap();
        }

        s.prune_at(&cfg, 3_000).unwrap();
        assert!(s.get(keep.id).is_ok(), "pinboard members are never pruned");
    }

    #[test]
    fn pinboards_manage_membership() {
        let s = store();
        let cfg = Config::default();
        let a = stored(s.insert(&NewClip::text("snippet a"), &cfg).unwrap());
        let b = stored(s.insert(&NewClip::text("snippet b"), &cfg).unwrap());

        s.create_pinboard("work").unwrap();
        // Creating twice is idempotent, not an error.
        s.create_pinboard("work").unwrap();
        assert_eq!(s.list_pinboards().unwrap().len(), 1);

        s.add_to_pinboard("work", a.id).unwrap();
        s.add_to_pinboard("work", b.id).unwrap();
        s.add_to_pinboard("work", b.id).unwrap();
        assert_eq!(s.pinboard_clips("work", 10).unwrap().len(), 2);
        assert_eq!(s.pinboard_by_name("work").unwrap().clip_count, 2);

        s.remove_from_pinboard("work", a.id).unwrap();
        assert_eq!(s.pinboard_clips("work", 10).unwrap().len(), 1);

        assert!(matches!(
            s.add_to_pinboard("work", 9_999),
            Err(Error::ClipNotFound(9_999))
        ));
        assert!(matches!(
            s.pinboard_clips("nope", 10),
            Err(Error::PinboardNotFound(_))
        ));
    }

    #[test]
    fn deleting_a_clip_removes_its_pinboard_rows() {
        let s = store();
        let cfg = Config::default();
        let clip = stored(s.insert(&NewClip::text("temp"), &cfg).unwrap());
        s.create_pinboard("board").unwrap();
        s.add_to_pinboard("board", clip.id).unwrap();

        s.delete(clip.id).unwrap();
        assert_eq!(s.pinboard_by_name("board").unwrap().clip_count, 0);
    }

    #[test]
    fn missing_ids_report_not_found() {
        let s = store();
        assert!(matches!(s.get(1), Err(Error::ClipNotFound(1))));
        assert!(matches!(s.touch(1), Err(Error::ClipNotFound(1))));
        assert!(matches!(s.delete(1), Err(Error::ClipNotFound(1))));
        assert!(matches!(s.set_pinned(1, true), Err(Error::ClipNotFound(1))));
        assert!(matches!(s.clip_bytes(1), Err(Error::ClipNotFound(1))));
    }

    #[test]
    fn stats_reflect_contents() {
        let s = store();
        let cfg = Config::default();
        let a = stored(s.insert(&NewClip::text("aaa"), &cfg).unwrap());
        s.insert(&NewClip::text("bbbb"), &cfg).unwrap();
        s.set_pinned(a.id, true).unwrap();
        s.create_pinboard("b").unwrap();

        let st = s.stats().unwrap();
        assert_eq!(st.total_clips, 2);
        assert_eq!(st.pinned_clips, 1);
        assert_eq!(st.pinboards, 1);
        assert_eq!(st.total_bytes, 7);
        assert!(st.oldest_created_at.is_some());
    }

    #[test]
    fn survives_reopen_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite3");
        let cfg = Config::default();
        {
            let s = Store::open(&path).unwrap();
            s.insert(&NewClip::text("persisted"), &cfg).unwrap();
            s.create_pinboard("kept").unwrap();
        }
        let s = Store::open(&path).unwrap();
        let clips = s.recent(10, 0).unwrap();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].text.as_deref(), Some("persisted"));
        assert_eq!(s.list_pinboards().unwrap().len(), 1);
        // Re-running migrations and the FTS probe on an existing file is a no-op.
        assert_eq!(s.search("persisted", 10).unwrap().len(), 1);
    }

    #[test]
    fn file_is_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite3");
        let s = Store::open(&path).unwrap();
        s.insert(&NewClip::text("secret"), &Config::default())
            .unwrap();
        drop(s);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "clipboard history must not be world-readable");
        }
    }

    #[test]
    fn recent_pages_with_offset() {
        let s = store();
        let cfg = Config::default();
        for i in 0..5 {
            s.insert_at(&NewClip::text(format!("c{i}")), &cfg, 1_000 + i)
                .unwrap();
        }
        let page = s.recent(2, 2).unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].text.as_deref(), Some("c2"));
    }

    #[test]
    fn filters_by_kind() {
        let s = store();
        let cfg = Config::default();
        s.insert(&NewClip::text("https://example.com"), &cfg)
            .unwrap();
        s.insert(&NewClip::text("#aabbcc"), &cfg).unwrap();
        s.insert(&NewClip::text("just words"), &cfg).unwrap();

        assert_eq!(s.by_kind(ClipKind::Link, 10).unwrap().len(), 1);
        assert_eq!(s.by_kind(ClipKind::Color, 10).unwrap().len(), 1);
        assert_eq!(s.by_kind(ClipKind::Text, 10).unwrap().len(), 1);
    }

    #[test]
    fn touch_bumps_recency() {
        let s = store();
        let cfg = Config::default();
        let first = stored(s.insert_at(&NewClip::text("first"), &cfg, 1_000).unwrap());
        s.insert_at(&NewClip::text("second"), &cfg, 2_000).unwrap();
        assert_eq!(s.recent(10, 0).unwrap()[0].text.as_deref(), Some("second"));

        s.touch(first.id).unwrap();
        assert_eq!(s.recent(10, 0).unwrap()[0].id, first.id);
    }

    #[test]
    fn touch_reorders_even_within_the_same_second() {
        // Regression: ordering used to key off last_used_at, which has
        // one-second resolution. Copying an older clip in the same second as
        // the newest one left it stuck below, so "paste from history moves it to
        // the top" visibly failed. Ordering keys off `seq` for this reason.
        let s = store();
        let cfg = Config::default();
        let now = 1_700_000_000;

        let first = stored(s.insert_at(&NewClip::text("older"), &cfg, now).unwrap());
        let second = stored(s.insert_at(&NewClip::text("newer"), &cfg, now).unwrap());
        assert_eq!(s.recent(10, 0).unwrap()[0].id, second.id);

        s.touch(first.id).unwrap();
        let order: Vec<i64> = s.recent(10, 0).unwrap().iter().map(|c| c.id).collect();
        assert_eq!(
            order,
            vec![first.id, second.id],
            "the touched clip must lead"
        );
    }

    #[test]
    fn dedupe_reorders_within_the_same_second_too() {
        let s = store();
        let cfg = Config::default();
        let now = 1_700_000_000;

        let older = stored(s.insert_at(&NewClip::text("recopied"), &cfg, now).unwrap());
        s.insert_at(&NewClip::text("something else"), &cfg, now)
            .unwrap();

        // Copying the first thing again must float it back to the top.
        s.insert_at(&NewClip::text("recopied"), &cfg, now).unwrap();
        assert_eq!(s.recent(10, 0).unwrap()[0].id, older.id);
    }
}
