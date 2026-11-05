use crate::model::{DbMeta, SearchMessage, Session};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Cursor,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone)]
pub struct Loaded {
    pub session: Session,
    pub search_text: Option<String>,
    pub search_messages: Vec<SearchMessage>,
}

pub struct Write {
    pub path: String,
    pub signature: String,
    pub loaded: Loaded,
}

pub fn fingerprint(path: &Path, db: Option<&DbMeta>) -> Result<String> {
    let m = fs::metadata(path)?;
    // Titles are decorated on every query; changing a title does not invalidate log text.
    let metadata = db.map(|d| {
        (
            &d.created_at,
            &d.cwd,
            &d.source,
            &d.repository_url,
            &d.branch,
        )
    });
    let hash = Sha256::digest(serde_json::to_vec(&metadata)?);
    Ok(format!(
        "{}:{}:{}:{}:{}:{hash:x}",
        m.dev(),
        m.ino(),
        m.len(),
        m.mtime(),
        m.mtime_nsec()
    ))
}

pub fn directory() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(dirs::cache_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("session-finder")
}

pub struct Cache {
    conn: Connection,
}

impl Cache {
    pub fn open(directory: &Path, home: &Path) -> Result<Self> {
        if !directory.exists() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(directory)?;
        }
        let hash = format!("{:x}", Sha256::digest(home.to_string_lossy().as_bytes()));
        let path = directory.join(format!("index-v1-{}.sqlite", &hash[..16]));
        // journal_mode transitions do not always honor SQLite's busy timeout.
        // Serialize only schema initialization; ordinary queries still use WAL.
        let init_lock = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path.with_extension("lock"))?;
        init_lock.lock()?;
        let _file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)?;
        let conn = Connection::open(&path)?;
        conn.busy_timeout(Duration::from_secs(2))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS files (
                path TEXT PRIMARY KEY, signature TEXT NOT NULL, level INTEGER NOT NULL, summary TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS texts (
                path TEXT PRIMARY KEY REFERENCES files(path) ON DELETE CASCADE,
                signature TEXT NOT NULL, body BLOB NOT NULL
            );
            CREATE TABLE IF NOT EXISTS search_messages_v1 (
                path TEXT PRIMARY KEY REFERENCES files(path) ON DELETE CASCADE,
                signature TEXT NOT NULL, body BLOB NOT NULL
            );
            CREATE TABLE IF NOT EXISTS relation_headers (
                path TEXT PRIMARY KEY, signature TEXT NOT NULL, header TEXT NOT NULL
            );"
        )?;
        Ok(Self { conn })
    }

    pub fn read_relation_header(
        &self,
        path: &str,
        signature: &str,
    ) -> Result<Option<crate::relations::HeaderScan>> {
        let data: Option<String> = self
            .conn
            .prepare_cached("SELECT header FROM relation_headers WHERE path=?1 AND signature=?2")?
            .query_row(params![path, signature], |r| r.get(0))
            .optional()?;
        data.map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }

    pub fn save_relation_headers(
        &mut self,
        writes: &[(String, String, crate::relations::HeaderScan)],
        live_paths: &[PathBuf],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        for (path, signature, header) in writes {
            tx.execute(
                "INSERT INTO relation_headers VALUES(?1,?2,?3)
                ON CONFLICT(path) DO UPDATE SET signature=excluded.signature,header=excluded.header",
                params![path, signature, serde_json::to_string(header)?],
            )?;
        }
        tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS live_relation_paths(path TEXT PRIMARY KEY); DELETE FROM live_relation_paths;")?;
        {
            let mut insert = tx.prepare("INSERT INTO live_relation_paths VALUES(?1)")?;
            for path in live_paths {
                insert.execute([path.to_string_lossy().as_ref()])?;
            }
        }
        tx.execute(
            "DELETE FROM relation_headers WHERE path NOT IN (SELECT path FROM live_relation_paths)",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn read(
        &self,
        path: &str,
        signature: &str,
        header: bool,
        full: bool,
    ) -> Result<Option<Loaded>> {
        let summary: Option<String> = self
            .conn
            .prepare_cached(
                "SELECT summary FROM files WHERE path=?1 AND signature=?2 AND level>=?3",
            )?
            .query_row(params![path, signature, i32::from(!header)], |r| r.get(0))
            .optional()?;
        let Some(summary) = summary else {
            return Ok(None);
        };
        let (search_text, search_messages) = if full {
            let data: Option<Vec<u8>> = self
                .conn
                .prepare_cached("SELECT body FROM texts WHERE path=?1 AND signature=?2")?
                .query_row(params![path, signature], |r| r.get(0))
                .optional()?;
            let Some(data) = data else { return Ok(None) };
            let text = String::from_utf8(zstd::stream::decode_all(Cursor::new(data))?)?;
            // Old normalized-only caches cannot supply original snippets. Refresh
            // their full-text rows lazily, while keeping summary/header reads valid.
            // Keep texts in its original format so older binaries can still read it.
            let data: Option<Vec<u8>> = self
                .conn
                .prepare_cached(
                    "SELECT body FROM search_messages_v1 WHERE path=?1 AND signature=?2",
                )?
                .query_row(params![path, signature], |r| r.get(0))
                .optional()?;
            let Some(data) = data else { return Ok(None) };
            let messages = serde_json::from_slice(&zstd::stream::decode_all(Cursor::new(data))?)?;
            (Some(text), messages)
        } else {
            (None, Vec::new())
        };
        Ok(Some(Loaded {
            session: serde_json::from_str(&summary)?,
            search_text,
            search_messages,
        }))
    }

    pub fn save(&mut self, writes: &[Write], live_paths: &[PathBuf]) -> Result<()> {
        // Compress outside the transaction so another invocation need not wait for compression.
        let compressed: Vec<_> = writes
            .iter()
            .map(|w| {
                w.loaded
                    .search_text
                    .as_ref()
                    .map(|s| -> Result<_> {
                        let data = serde_json::to_vec(&w.loaded.search_messages)?;
                        Ok((
                            zstd::stream::encode_all(Cursor::new(s), 1)?,
                            zstd::stream::encode_all(Cursor::new(data), 1)?,
                        ))
                    })
                    .transpose()
            })
            .collect::<Result<_>>()?;
        let tx = self.conn.transaction()?;
        for (w, body) in writes.iter().zip(compressed.iter()) {
            tx.execute(
                "DELETE FROM texts WHERE path=?1 AND signature<>?2",
                params![w.path, w.signature],
            )?;
            tx.execute(
                "DELETE FROM search_messages_v1 WHERE path=?1 AND signature<>?2",
                params![w.path, w.signature],
            )?;
            tx.execute("INSERT INTO files(path,signature,level,summary) VALUES(?1,?2,?3,?4)
                ON CONFLICT(path) DO UPDATE SET signature=excluded.signature,level=excluded.level,summary=excluded.summary
                WHERE files.signature<>excluded.signature OR excluded.level>=files.level",
                params![w.path, w.signature, i32::from(w.loaded.session.message_count.is_some()), serde_json::to_string(&w.loaded.session)?])?;
            if let Some((body, messages)) = body {
                tx.execute("INSERT INTO texts(path,signature,body) VALUES(?1,?2,?3)
                    ON CONFLICT(path) DO UPDATE SET signature=excluded.signature,body=excluded.body",
                    params![w.path, w.signature, body])?;
                tx.execute("INSERT INTO search_messages_v1(path,signature,body) VALUES(?1,?2,?3)
                    ON CONFLICT(path) DO UPDATE SET signature=excluded.signature,body=excluded.body",
                    params![w.path, w.signature, messages])?;
            }
        }
        // Only remove logs absent from the full filesystem listing; scoped refreshes preserve other projects.
        tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS live_paths(path TEXT PRIMARY KEY); DELETE FROM live_paths;")?;
        {
            let mut insert = tx.prepare("INSERT INTO live_paths VALUES(?1)")?;
            for path in live_paths {
                insert.execute([path.to_string_lossy().as_ref()])?;
            }
        }
        tx.execute(
            "DELETE FROM files WHERE path NOT IN (SELECT path FROM live_paths)",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }
}
