//! Orka persistence (Milestone 2): minimal SQLite store for last-applied.
//! Keep code tiny and predictable.

#![forbid(unsafe_code)]

use anyhow::{Context, Result};
use std::io::Seek;
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


/// Append-only log store (pure Rust). Format per record:
/// [ts_i64][uid[16]][rv_len_u32][yaml_len_u32][rv_bytes][yaml_bytes]
pub struct LogStore {
    file: std::sync::Mutex<std::fs::File>,
    index: std::sync::Mutex<std::collections::HashMap<[u8;16], Vec<u64>>>,
    path: String,
}

impl LogStore {
    pub fn open_default() -> Result<Self> {
        let path = std::env::var("ORKA_DB_PATH").unwrap_or_else(|_| default_log_path());
        Self::open(&path)
    }

    pub fn open(path: &str) -> Result<Self> {
        let started = std::time::Instant::now();
        let f = std::fs::OpenOptions::new().read(true).write(true).create(true).append(true).open(path)
            .with_context(|| format!("opening log store at {}", path))?;
        let mut idx: std::collections::HashMap<[u8;16], Vec<u64>> = std::collections::HashMap::new();
        // Walk the file to build in-memory offsets per uid
        let mut rf = std::fs::File::open(path)?;
        let mut off: u64 = 0;
        let mut buf8 = [0u8; 8];
        let mut uid = [0u8; 16];
        loop {
            // ts (8 bytes)
            match std::io::Read::read_exact(&mut rf, &mut buf8) {
                Ok(()) => {}
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::UnexpectedEof { break; } else { return Err(e.into()); }
                }
            }
            let ts_le = i64::from_le_bytes(buf8);
            let _ = ts_le; // not used here
            off += 8;
            // uid
            std::io::Read::read_exact(&mut rf, &mut uid)?;
            off += 16;
            // rv_len
            std::io::Read::read_exact(&mut rf, &mut buf8[..4])?;
            let rv_len = u32::from_le_bytes([buf8[0], buf8[1], buf8[2], buf8[3]]) as usize;
            off += 4;
            // yaml_len
            std::io::Read::read_exact(&mut rf, &mut buf8[..4])?;
            let yaml_len = u32::from_le_bytes([buf8[0], buf8[1], buf8[2], buf8[3]]) as usize;
            off += 4;
            // skip rv + yaml
            let skip = (rv_len + yaml_len) as u64;
            // Record offset points to start of this record
            let rec_off = off - 8 - 16 - 4 - 4;
            let v = idx.entry(uid).or_default();
            v.push(rec_off);
            // Seek forward
            rf.seek(std::io::SeekFrom::Current(skip as i64))?;
            off += skip;
        }
        histogram!("persist_open_ms", started.elapsed().as_secs_f64() * 1000.0);
        Ok(Self { file: std::sync::Mutex::new(f), index: std::sync::Mutex::new(idx), path: path.to_string() })
    }
}

impl Store for LogStore {
    fn put_last(&self, la: LastApplied) -> Result<()> {
        let started = std::time::Instant::now();
        let mut f = self.file.lock().unwrap();
        // Prepare buffers
        let mut rec: Vec<u8> = Vec::with_capacity(8 + 16 + 4 + 4 + la.rv.len() + la.yaml_zstd.len());
        rec.extend_from_slice(&la.ts.to_le_bytes());
        rec.extend_from_slice(&la.uid);
        rec.extend_from_slice(&(la.rv.len() as u32).to_le_bytes());
        rec.extend_from_slice(&(la.yaml_zstd.len() as u32).to_le_bytes());
        rec.extend_from_slice(la.rv.as_bytes());
        rec.extend_from_slice(&la.yaml_zstd);
        // Append and flush
        let off = f.metadata()?.len();
        std::io::Write::write_all(&mut *f, &rec)?;
        std::io::Write::flush(&mut *f)?;
        // Update index
        let mut idx = self.index.lock().unwrap();
        idx.entry(la.uid).or_default().push(off);
        // Optional prune vector size to last few offsets (index only)
        if let Some(v) = idx.get_mut(&la.uid) { if v.len() > 64 { let keep = v.split_off(v.len() - 64); *v = keep; } }
        histogram!("persist_put_ms", started.elapsed().as_secs_f64() * 1000.0);
        counter!("persist_put_total", 1u64);
        Ok(())
    }

    fn get_last(&self, uid: [u8; 16], limit: Option<usize>) -> Result<Vec<LastApplied>> {
        let started = std::time::Instant::now();
        let cap = limit.unwrap_or(3);
        let idx = self.index.lock().unwrap();
        let offs = idx.get(&uid).cloned().unwrap_or_default();
        drop(idx);
        let mut rf = std::fs::File::open(&self.path)?;
        let mut out: Vec<LastApplied> = Vec::new();
        for off in offs.into_iter().rev().take(cap) {
            rf.seek(std::io::SeekFrom::Start(off))?;
            // ts
            let mut buf8 = [0u8; 8];
            std::io::Read::read_exact(&mut rf, &mut buf8)?;
            let ts = i64::from_le_bytes(buf8);
            // uid
            let mut u = [0u8; 16];
            std::io::Read::read_exact(&mut rf, &mut u)?;
            // rv_len
            std::io::Read::read_exact(&mut rf, &mut buf8[..4])?;
            let rv_len = u32::from_le_bytes([buf8[0], buf8[1], buf8[2], buf8[3]]) as usize;
            // yaml_len
            std::io::Read::read_exact(&mut rf, &mut buf8[..4])?;
            let yaml_len = u32::from_le_bytes([buf8[0], buf8[1], buf8[2], buf8[3]]) as usize;
            // rv
            let mut rv_bytes = vec![0u8; rv_len];
            std::io::Read::read_exact(&mut rf, &mut rv_bytes)?;
            // yaml
            let mut yaml_bytes = vec![0u8; yaml_len];
            std::io::Read::read_exact(&mut rf, &mut yaml_bytes)?;
            out.push(LastApplied { uid, rv: String::from_utf8_lossy(&rv_bytes).to_string(), ts, yaml_zstd: yaml_bytes });
        }
        histogram!("persist_get_ms", started.elapsed().as_secs_f64() * 1000.0);
        Ok(out)
    }
}


fn default_log_path() -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = std::path::PathBuf::from(home);
        p.push(".orka");
        let _ = std::fs::create_dir_all(&p);
        p.push("lastapplied.log");
        return p.to_string_lossy().to_string();
    }
    "lastapplied.log".to_string()
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

    // sqlite removed

    fn temp_log() -> String {
        let dir = std::env::temp_dir();
        let f = format!("orka-test-{}.log", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
        dir.join(f).to_string_lossy().to_string()
    }

    #[test]
    fn put_get_logstore() {
        let path = temp_log();
        let s = LogStore::open(&path).unwrap();
        let uid = [9u8; 16];
        for i in 0..5 {
            let la = LastApplied { uid, rv: format!("rv-{}", i), ts: i as i64, yaml_zstd: maybe_compress(&format!("k: v{}\n", i)) };
            s.put_last(la).unwrap();
        }
        let rows = s.get_last(uid, Some(3)).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].rv, "rv-4");
        assert_eq!(rows[1].rv, "rv-3");
        assert_eq!(rows[2].rv, "rv-2");
    }
}
