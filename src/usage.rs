//! One JSON line per run in `<state>/usage.jsonl` (spec §8), for the two-week review (§10).
//! Local only, never sent anywhere. The host is a short hash, never the name.

use crate::kind::Kind;
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;
use std::time::SystemTime;

#[derive(Debug, Clone, Default)]
pub struct Record {
    /// `[user@]host` label of a remote target; `None` for a local one.
    pub host: Option<String>,
    pub kind: Option<Kind>,
    pub ext: Option<String>,
    pub bytes: u64,
    pub fetch_ms: u64,
    /// `open` | `save` | `export` | `cut` | `discard`.
    pub action: Option<&'static str>,
    pub conflict: bool,
    /// False when the run ended in an error (§10: a failed write is fixed before anything else).
    pub ok: bool,
}

/// First 12 hex characters of SHA-256 over the host label: stable per host, not reversible to
/// a name by reading the log.
pub fn host_hash(host: &str) -> String {
    let digest = Sha256::digest(format!("sopen-host:{host}").as_bytes());
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

pub fn line(rec: &Record, at: SystemTime) -> String {
    serde_json::json!({
        "ts": crate::cache::rfc3339(at),
        "host_hash": rec.host.as_deref().map(host_hash).unwrap_or_else(|| "local".to_string()),
        "kind": rec.kind.map(Kind::as_str),
        "ext": rec.ext,
        "bytes": rec.bytes,
        "fetch_ms": rec.fetch_ms,
        "action": rec.action,
        "conflict": rec.conflict,
        "ok": rec.ok,
    })
    .to_string()
}

pub fn append(state_dir: &Path, rec: &Record) -> Result<()> {
    std::fs::create_dir_all(state_dir)
        .with_context(|| format!("cannot create {}", state_dir.display()))?;
    let path = state_dir.join("usage.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    writeln!(file, "{}", line(rec, SystemTime::now()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn line_has_spec_fields_and_no_host_name() {
        let rec = Record {
            host: Some("me@build-box".into()),
            kind: Some(Kind::Audio),
            ext: Some("mp3".into()),
            bytes: 1234,
            fetch_ms: 56,
            action: Some("cut"),
            conflict: false,
            ok: true,
        };
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(86_400);
        let text = line(&rec, at);
        assert!(!text.contains("build-box"), "{text}");
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["ts"], "1970-01-02T00:00:00Z");
        assert_eq!(v["host_hash"].as_str().unwrap().len(), 12);
        assert_eq!(v["kind"], "audio");
        assert_eq!(v["ext"], "mp3");
        assert_eq!(v["bytes"], 1234);
        assert_eq!(v["fetch_ms"], 56);
        assert_eq!(v["action"], "cut");
        assert_eq!(v["conflict"], false);
        assert_eq!(host_hash("a"), host_hash("a"));
        assert_ne!(host_hash("a"), host_hash("b"));
    }

    #[test]
    fn append_writes_one_line_per_run() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = Record::default();
        append(tmp.path(), &rec).unwrap();
        append(tmp.path(), &rec).unwrap();
        let text = std::fs::read_to_string(tmp.path().join("usage.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("\"host_hash\":\"local\""));
    }
}
