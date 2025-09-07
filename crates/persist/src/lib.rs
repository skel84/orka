//! Orka persistence (Milestone 2): minimal SQLite store for last-applied.
//! Keep code tiny and predictable.

#![forbid(unsafe_code)]

use anyhow::{Context, Result};
use metrics::{counter, histogram};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastApplied {
    pub uid: [u8; 16],
    pub rv: String,
    pub ts: i64,
    pub yaml_zstd: Vec<u8>,
}

pub trait Store {
    fn put_last(&self, la: LastApplied) -> Result<()>;
    fn get_last(&self, uid: [u8; 16], limit: Option<usize>) -> Result<Vec<LastApplied>>;
}

/// SQLite-backed store. Simple, synchronous. The CLI isn’t latency sensitive here.
pub struct SqliteStore {
    db: std::sync::Mutex<rusqlite::Connection>,
}

impl SqliteStore {
    pub fn open_default() -> Result<Self> {
        let path = std::env::var("ORKA_DB_PATH").unwrap_or_else(|_| default_db_path());
        Self::open(&path)
    }

    pub fn open(path: &str) -> Result<Self> {
        let started = std::time::Instant::now();
        let db = rusqlite::Connection::open(path).with_context(|| format!("opening sqlite db at {}", path))?;
        db.pragma_update(None, "journal_mode", &"WAL").ok();
        db.pragma_update(None, "synchronous", &"NORMAL").ok();
        db.execute(
            "CREATE TABLE IF NOT EXISTS last_applied (
                uid  BLOB NOT NULL,
                rv   TEXT NOT NULL,
                ts   INTEGER NOT NULL,
                yaml BLOB NOT NULL
            )",
            [],
        ).context("creating last_applied table")?;
        db.execute(
            "CREATE INDEX IF NOT EXISTS idx_last_applied_uid_ts ON last_applied(uid, ts DESC)",
            [],
        ).ok();
        let me = Self { db: std::sync::Mutex::new(db) };
        histogram!("persist_open_ms", started.elapsed().as_secs_f64() * 1000.0);
        Ok(me)
    }
}

impl Store for SqliteStore {
    fn put_last(&self, la: LastApplied) -> Result<()> {
        let started = std::time::Instant::now();
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        tx.execute(
            "INSERT INTO last_applied(uid, rv, ts, yaml) VALUES (?1, ?2, ?3, ?4)",
            (
                &la.uid[..],
                &la.rv,
                la.ts,
                &la.yaml_zstd,
            ),
        )?;
        // Keep latest 3 by ts per uid (delete older rows by rowid)
        tx.execute(
            "DELETE FROM last_applied
             WHERE uid = ?1
               AND rowid NOT IN (
                   SELECT rowid FROM last_applied WHERE uid = ?1 ORDER BY ts DESC, rowid DESC LIMIT 3
               )",
            [&la.uid[..]],
        )?;
        tx.commit()?;
        histogram!("persist_put_ms", started.elapsed().as_secs_f64() * 1000.0);
        counter!("persist_put_total", 1u64);
        Ok(())
    }

    fn get_last(&self, uid: [u8; 16], limit: Option<usize>) -> Result<Vec<LastApplied>> {
        let started = std::time::Instant::now();
        let cap = limit.unwrap_or(3);
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT rv, ts, yaml FROM last_applied WHERE uid = ?1 ORDER BY ts DESC, rowid DESC LIMIT ?2",
        )?;
        let mut rows = stmt.query((uid.as_slice(), cap as i64))?;
        let mut out: Vec<LastApplied> = Vec::new();
        while let Some(row) = rows.next()? {
            let rv: String = row.get(0)?;
            let ts: i64 = row.get(1)?;
            let yaml: Vec<u8> = row.get(2)?;
            out.push(LastApplied { uid, rv, ts, yaml_zstd: yaml });
        }
        histogram!("persist_get_ms", started.elapsed().as_secs_f64() * 1000.0);
        Ok(out)
    }
}

fn default_db_path() -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = std::path::PathBuf::from(home);
        p.push(".orka");
        let _ = std::fs::create_dir_all(&p);
        p.push("orka.db");
        return p.to_string_lossy().to_string();
    }
    // Fallback to current directory
    "orka.db".to_string()
}

pub fn now_ts() -> i64 {
    // seconds since epoch
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    now.as_secs() as i64
}

pub fn maybe_compress(yaml: &str) -> Vec<u8> {
    #[cfg(feature = "zstd")]
    {
        let lvl: i32 = std::env::var("ORKA_ZSTD_LEVEL").ok().and_then(|s| s.parse().ok()).unwrap_or(3);
        return zstd::encode_all(yaml.as_bytes(), lvl as i32).unwrap_or_else(|_| yaml.as_bytes().to_vec());
    }
    yaml.as_bytes().to_vec()
}

pub fn maybe_decompress(blob: &[u8]) -> String {
    #[cfg(feature = "zstd")]
    {
        if let Ok(de) = zstd::decode_all(std::io::Cursor::new(blob)) {
            return String::from_utf8_lossy(&de).to_string();
        }
    }
    String::from_utf8_lossy(blob).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> String {
        let dir = std::env::temp_dir();
        let f = format!("orka-test-{}.db", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
        dir.join(f).to_string_lossy().to_string()
    }

    #[test]
    fn put_get_rotate() {
        let path = temp_db();
        let s = SqliteStore::open(&path).unwrap();
        let uid = [7u8; 16];
        for i in 0..5 {
            let la = LastApplied { uid, rv: format!("rv-{}", i), ts: i as i64, yaml_zstd: maybe_compress(&format!("k: v{}\n", i)) };
            s.put_last(la).unwrap();
        }
        let rows = s.get_last(uid, None).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].rv, "rv-4");
        assert_eq!(rows[1].rv, "rv-3");
        assert_eq!(rows[2].rv, "rv-2");
    }
}
