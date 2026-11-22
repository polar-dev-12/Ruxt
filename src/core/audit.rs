//! Append-only audit log (JSON lines). Never contains secrets.
//!
//! The file is created owner-only from the start and rotated once (`audit.log.1`) when it
//! grows past `MAX_BYTES`, so a SOCKS-heavy session cannot fill the disk.
//!
//! It is plain text on purpose: it holds metadata (hosts, users, paths, times), not secrets,
//! and it is meant to be read by its owner. Disk-level protection is left to BitLocker /
//! FileVault / LUKS. Logging can be switched off in Settings; the switch itself is recorded.

use parking_lot::Mutex;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

static AUDIT: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(true);

/// Rotate when the current log exceeds this size.
const MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Start logging to `path`. Lines older than `retain_days` are dropped first (0 = keep all),
/// so the log does not become a permanent record of every host ever touched.
pub fn init(path: PathBuf, retain_days: u64, enabled: bool) {
    if retain_days > 0 {
        prune(&path, retain_days);
    }
    ENABLED.store(enabled, Ordering::Relaxed);
    let _ = AUDIT.set(Mutex::new(Some(path)));
}

/// Turn logging on or off at run time. The change is the last (or first) line written, so
/// a gap in the log is explained by the log itself.
pub fn set_enabled(on: bool) {
    if on == ENABLED.load(Ordering::Relaxed) {
        return;
    }
    if on {
        ENABLED.store(true, Ordering::Relaxed);
        log("audit.enabled", serde_json::json!({}));
    } else {
        log("audit.disabled", serde_json::json!({}));
        ENABLED.store(false, Ordering::Relaxed);
    }
}

/// Rewrite the log keeping only lines whose `ts` is within `days`. Lines that cannot be
/// parsed are kept (never silently destroy evidence).
pub fn prune(path: &std::path::Path, days: u64) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let cutoff = chrono::Local::now() - chrono::Duration::days(days as i64);
    let mut kept = String::with_capacity(text.len());
    let mut dropped = 0usize;
    for line in text.lines() {
        let old = serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|v| v.get("ts").and_then(|t| t.as_str()).map(str::to_string))
            .and_then(|ts| chrono::DateTime::parse_from_rfc3339(&ts).ok())
            .map(|ts| ts < cutoff)
            .unwrap_or(false);
        if old {
            dropped += 1;
        } else {
            kept.push_str(line);
            kept.push('\n');
        }
    }
    if dropped > 0 && super::config::write_private_atomic(path, kept.as_bytes()).is_ok() {
        tracing::info!("audit log: dropped {dropped} line(s) older than {days} days");
    }
}

pub fn log(event: &str, fields: serde_json::Value) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let Some(lock) = AUDIT.get() else { return };
    let guard = lock.lock();
    let Some(path) = guard.as_ref() else { return };
    let mut obj = serde_json::Map::new();
    obj.insert(
        "ts".into(),
        serde_json::Value::String(chrono::Local::now().to_rfc3339()),
    );
    obj.insert("event".into(), serde_json::Value::String(event.to_string()));
    if let serde_json::Value::Object(m) = fields {
        for (k, v) in m {
            obj.insert(k, v);
        }
    }
    let line = serde_json::Value::Object(obj).to_string();
    if std::fs::metadata(path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let rotated = path.with_extension("log.1");
        let _ = std::fs::remove_file(&rotated);
        let _ = std::fs::rename(path, &rotated);
    }
    let res = super::config::ensure_private_file(path).and_then(|_| {
        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .and_then(|mut f| writeln!(f, "{line}"))
    });
    if let Err(e) = res {
        tracing::warn!("audit log write failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_drops_old_lines_and_keeps_unparsable_ones() {
        let dir = std::env::temp_dir().join(format!("ruxt-audit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("audit.log");
        let old = (chrono::Local::now() - chrono::Duration::days(400)).to_rfc3339();
        let new = chrono::Local::now().to_rfc3339();
        let text = format!(
            "{{\"ts\":\"{old}\",\"event\":\"old\"}}\n{{\"ts\":\"{new}\",\"event\":\"new\"}}\nnot json at all\n"
        );
        std::fs::write(&p, text).unwrap();
        prune(&p, 180);
        let kept = std::fs::read_to_string(&p).unwrap();
        assert!(!kept.contains("\"old\""));
        assert!(kept.contains("\"new\""));
        assert!(kept.contains("not json at all"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
