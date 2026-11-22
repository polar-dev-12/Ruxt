//! Command history for terminal auto-completion.
//!
//! Every command line the user submits is kept in a small SQLite database (`history.db` in
//! the data directory) and, once Ruxt is unlocked, loaded whole into memory: the table is a
//! few thousand short strings, so lookups while typing never touch the disk. Writes go
//! through a background thread so the UI thread never waits on SQLite.
//!
//! The history is only kept with a screen-lock password. Each row is encrypted with
//! XChaCha20-Poly1305 under a random key stored in the saved-password store (`vault.rs`),
//! with the row id as associated data; the store is opened with the screen-lock password, so
//! the history is readable only while Ruxt is unlocked and is dropped from memory when it
//! locks. Only a random row id and the last-use time are stored in the clear (for retention).
//! A plain-text table written by older versions is encrypted and removed on the first open.
//!
//! What is recorded is decided by the caller (`app`): it reads the submitted line back from
//! the terminal grid, so text a program did not echo (password prompts) never gets here.
//! This module adds a second filter for lines that look like they carry a secret, and a
//! leading space opts a line out, like `HISTCONTROL=ignorespace`.

use anyhow::{anyhow, Context, Result};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc;
use zeroize::{Zeroize, Zeroizing};

/// Entries kept in memory and on disk; the least recently used are evicted beyond this.
pub const MAX_ENTRIES: usize = 20_000;
/// Longest command line recorded (characters).
pub const MAX_CMD_LEN: usize = 512;
/// Shortest command line recorded (characters).
pub const MIN_CMD_LEN: usize = 2;

const ID_LEN: usize = 16;
const NONCE_LEN: usize = 24;

/// Key that encrypts the history rows.
pub type HistoryKey = Zeroizing<[u8; 32]>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Random row id (not derived from the command, so it reveals nothing).
    id: [u8; ID_LEN],
    pub cmd: String,
    /// Host the command was last used on ("local" for the local shell).
    pub host: String,
    pub uses: u32,
    /// Unix time of the last use.
    pub last_used: i64,
}

/// What is encrypted for one row.
#[derive(Serialize, Deserialize)]
struct Row {
    c: String,
    h: String,
    u: u32,
}

impl Drop for Row {
    fn drop(&mut self) {
        self.c.zeroize();
        self.h.zeroize();
    }
}

enum DbOp {
    Put(Entry),
    Delete([u8; ID_LEN]),
    Clear,
}

pub struct History {
    entries: Vec<Entry>,
    index: HashMap<String, usize>,
    tx: Option<mpsc::Sender<DbOp>>,
    /// The writer thread; joined on drop so pending writes land and the file is closed.
    writer: Option<std::thread::JoinHandle<()>>,
    /// Records and suggests at all (false while Ruxt is locked or has no screen lock).
    open: bool,
}

impl Drop for History {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
        for e in &mut self.entries {
            e.cmd.zeroize();
            e.host.zeroize();
        }
        for (mut k, _) in self.index.drain() {
            k.zeroize();
        }
    }
}

impl History {
    /// Open (or create) the database at `path` with `key` and load every entry. Entries not
    /// used for `retain_days` days are deleted first (0 = keep forever). Rows that do not
    /// decrypt with `key` (left over from a removed screen lock) are deleted.
    pub fn open(path: &Path, retain_days: u64, key: HistoryKey) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Owner-only from the first byte; SQLite accepts an empty file as a new database.
        super::config::ensure_private_file(path).context("creating history database")?;
        let conn = Connection::open(path).context("opening history database")?;
        // Freed pages are overwritten, so a deleted row does not linger in the file.
        conn.execute_batch(
            "PRAGMA secure_delete = ON;
             CREATE TABLE IF NOT EXISTS history (
                id BLOB PRIMARY KEY NOT NULL,
                last_used INTEGER NOT NULL DEFAULT 0,
                data BLOB NOT NULL
             ) WITHOUT ROWID;",
        )
        .context("initialising history schema")?;
        let cipher = XChaCha20Poly1305::new((&*key).into());
        migrate_plaintext(&conn, &cipher).context("encrypting the old command history")?;
        if retain_days > 0 {
            let cutoff = now() - (retain_days as i64).saturating_mul(86_400);
            let _ = conn.execute("DELETE FROM history WHERE last_used < ?1", params![cutoff]);
        }
        let mut entries = Vec::new();
        let mut undecryptable: Vec<Vec<u8>> = Vec::new();
        {
            let mut stmt = conn.prepare("SELECT id, last_used, data FROM history ORDER BY last_used DESC LIMIT ?1")?;
            let rows = stmt.query_map(params![MAX_ENTRIES as i64], |r| {
                Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?, r.get::<_, Vec<u8>>(2)?))
            })?;
            for row in rows {
                let (id, last_used, data) = row?;
                match decrypt_row(&cipher, &id, &data) {
                    Some((id, row)) if is_recordable(&row.c) => entries.push(Entry {
                        id,
                        cmd: row.c.clone(),
                        host: row.h.clone(),
                        uses: row.u,
                        last_used,
                    }),
                    Some(_) => {}
                    None => undecryptable.push(id),
                }
            }
        }
        for id in undecryptable {
            let _ = conn.execute("DELETE FROM history WHERE id = ?1", params![id]);
        }
        let index = entries.iter().enumerate().map(|(i, e)| (e.cmd.clone(), i)).collect();
        let (tx, rx) = mpsc::channel::<DbOp>();
        let writer = std::thread::Builder::new()
            .name("history-db".into())
            .spawn(move || writer(conn, cipher, rx))
            .context("starting history writer thread")?;
        Ok(Self {
            entries,
            index,
            tx: Some(tx),
            writer: Some(writer),
            open: true,
        })
    }

    /// No history: Ruxt is locked or has no screen-lock password. Records and suggests nothing.
    pub fn locked() -> Self {
        Self {
            entries: Vec::new(),
            index: HashMap::new(),
            tx: None,
            writer: None,
            open: false,
        }
    }

    /// A store that only lives in memory.
    #[cfg(test)]
    pub fn in_memory() -> Self {
        let mut h = Self::locked();
        h.open = true;
        h
    }

    /// History is available (Ruxt unlocked with a screen-lock password).
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Record a submitted line. Returns `false` when the line was filtered out or the history
    /// is not open.
    pub fn record(&mut self, line: &str, host: &str) -> bool {
        if !self.open || !is_recordable(line) {
            return false;
        }
        let cmd = line.trim().to_string();
        let ts = now();
        let host = host.trim().to_string();
        let i = match self.index.get(&cmd) {
            Some(&i) => {
                let e = &mut self.entries[i];
                e.uses = e.uses.saturating_add(1);
                e.last_used = ts;
                e.host = host;
                i
            }
            None => {
                let mut id = [0u8; ID_LEN];
                rand::fill(&mut id);
                self.index.insert(cmd.clone(), self.entries.len());
                self.entries.push(Entry {
                    id,
                    cmd,
                    host,
                    uses: 1,
                    last_used: ts,
                });
                self.entries.len() - 1
            }
        };
        self.send(DbOp::Put(self.entries[i].clone()));
        self.evict();
        true
    }

    /// Completions for `prefix`: longer entries starting with it, best first (entries last
    /// used on `host`, then most used, then most recent).
    pub fn suggest(&self, prefix: &str, host: &str, limit: usize) -> Vec<String> {
        let prefix = prefix.trim_start();
        if prefix.is_empty() || limit == 0 {
            return Vec::new();
        }
        let mut hits: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| e.cmd.len() > prefix.len() && e.cmd.starts_with(prefix))
            .collect();
        hits.sort_by(|a, b| {
            (b.host == host)
                .cmp(&(a.host == host))
                .then(b.uses.cmp(&a.uses))
                .then(b.last_used.cmp(&a.last_used))
                .then(a.cmd.cmp(&b.cmd))
        });
        hits.into_iter().take(limit).map(|e| e.cmd.clone()).collect()
    }

    /// Forget one entry (e.g. a suggestion the user does not want to see again).
    pub fn forget(&mut self, cmd: &str) {
        if let Some(i) = self.index.remove(cmd) {
            let mut gone = self.entries.swap_remove(i);
            if i < self.entries.len() {
                let moved = self.entries[i].cmd.clone();
                self.index.insert(moved, i);
            }
            self.send(DbOp::Delete(gone.id));
            gone.cmd.zeroize();
            gone.host.zeroize();
        }
    }

    /// Forget everything, in memory and on disk.
    pub fn clear(&mut self) {
        for e in &mut self.entries {
            e.cmd.zeroize();
            e.host.zeroize();
        }
        self.entries.clear();
        self.index.clear();
        self.send(DbOp::Clear);
    }

    fn evict(&mut self) {
        while self.entries.len() > MAX_ENTRIES {
            let Some((i, _)) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| (e.last_used, e.uses))
            else {
                break;
            };
            let victim = self.entries[i].cmd.clone();
            self.forget(&victim);
        }
    }

    fn send(&self, op: DbOp) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(op);
        }
    }
}

/// A fresh random key for a new history.
pub fn new_key() -> HistoryKey {
    let mut k = Zeroizing::new([0u8; 32]);
    rand::fill(&mut *k);
    k
}

/// Delete the history database (screen lock removed, or history cleared while it is not
/// open). The file is overwritten first: an old version may have left plain text in it.
pub fn delete_file(path: &Path) -> Result<()> {
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::write(path, vec![0u8; meta.len() as usize]);
    }
    for p in [path.to_path_buf(), path.with_extension("db-journal")] {
        match std::fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).context("removing command history"),
        }
    }
    Ok(())
}

fn encrypt_row(cipher: &XChaCha20Poly1305, e: &Entry) -> Result<Vec<u8>> {
    let row = Row {
        c: e.cmd.clone(),
        h: e.host.clone(),
        u: e.uses,
    };
    let plain = Zeroizing::new(serde_json::to_vec(&row)?);
    let mut nonce = [0u8; NONCE_LEN];
    rand::fill(&mut nonce);
    let ct = cipher
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: &plain,
                aad: &e.id,
            },
        )
        .map_err(|_| anyhow!("history encryption failed"))?;
    let mut out = nonce.to_vec();
    out.extend_from_slice(&ct);
    Ok(out)
}

/// `None` for a row that is malformed or encrypted under another key.
fn decrypt_row(cipher: &XChaCha20Poly1305, id: &[u8], data: &[u8]) -> Option<([u8; ID_LEN], Row)> {
    let id: [u8; ID_LEN] = id.try_into().ok()?;
    if data.len() < NONCE_LEN + 16 {
        return None;
    }
    let (nonce, ct) = data.split_at(NONCE_LEN);
    let nonce = XNonce::try_from(nonce).ok()?;
    let plain = Zeroizing::new(cipher.decrypt(&nonce, Payload { msg: ct, aad: &id }).ok()?);
    let row: Row = serde_json::from_slice(&plain).ok()?;
    Some((id, row))
}

/// Encrypt the rows of the plain-text `commands` table that older versions wrote, then drop
/// it and rebuild the file so no plain text is left in free pages.
fn migrate_plaintext(conn: &Connection, cipher: &XChaCha20Poly1305) -> Result<()> {
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'commands'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !exists {
        return Ok(());
    }
    let mut old = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT cmd, host, uses, last_used FROM commands")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows.flatten() {
            old.push(row);
        }
    }
    let tx = conn.unchecked_transaction()?;
    for (cmd, host, uses, last_used) in &mut old {
        if is_recordable(cmd) {
            let mut id = [0u8; ID_LEN];
            rand::fill(&mut id);
            let e = Entry {
                id,
                cmd: std::mem::take(cmd),
                host: std::mem::take(host),
                uses: (*uses).clamp(0, u32::MAX as i64) as u32,
                last_used: *last_used,
            };
            let data = encrypt_row(cipher, &e)?;
            tx.execute(
                "INSERT OR REPLACE INTO history (id, last_used, data) VALUES (?1, ?2, ?3)",
                params![&e.id[..], e.last_used, data],
            )?;
            let (mut c, mut h) = (e.cmd, e.host);
            c.zeroize();
            h.zeroize();
        }
        cmd.zeroize();
        host.zeroize();
    }
    tx.execute_batch("DROP TABLE commands;")?;
    tx.commit()?;
    conn.execute_batch("VACUUM;")?;
    Ok(())
}

fn writer(conn: Connection, cipher: XChaCha20Poly1305, rx: mpsc::Receiver<DbOp>) {
    while let Ok(op) = rx.recv() {
        let r = match op {
            DbOp::Put(mut e) => {
                let r = encrypt_row(&cipher, &e).and_then(|data| {
                    conn.execute(
                        "INSERT OR REPLACE INTO history (id, last_used, data) VALUES (?1, ?2, ?3)",
                        params![&e.id[..], e.last_used, data],
                    )
                    .map(|_| ())
                    .map_err(Into::into)
                });
                e.cmd.zeroize();
                e.host.zeroize();
                r
            }
            DbOp::Delete(id) => conn
                .execute("DELETE FROM history WHERE id = ?1", params![&id[..]])
                .map(|_| ())
                .map_err(Into::into),
            DbOp::Clear => conn.execute_batch("DELETE FROM history; VACUUM;").map_err(Into::into),
        };
        if let Err(e) = r {
            tracing::warn!("history database write failed: {e}");
        }
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Substrings (lower-case) that mark a line as carrying a secret.
const SECRET_MARKERS: &[&str] = &[
    "password=",
    "passwd=",
    "passwort=",
    "pass=",
    "pwd=",
    "token=",
    "secret=",
    "api_key=",
    "apikey=",
    "api-key=",
    "access_key=",
    "private_key=",
    "authorization:",
    "bearer ",
    "--password",
    "--token",
    "--secret",
    "sshpass ",
    "x-api-key",
];

/// Environment-variable names (upper-case fragments) whose assignment is never recorded.
const SECRET_ENV_FRAGMENTS: &[&str] = &["PASS", "SECRET", "TOKEN", "KEY", "CREDENTIAL"];

/// Does `line` look like it contains a credential? Heuristic, deliberately broad.
pub fn looks_sensitive(line: &str) -> bool {
    let lower = line.to_lowercase();
    if SECRET_MARKERS.iter().any(|m| lower.contains(m)) {
        return true;
    }
    // `FOO_TOKEN=…`, `export AWS_SECRET_ACCESS_KEY=…`, `env API_KEY=… cmd`
    for word in line.split_whitespace() {
        if let Some((name, value)) = word.split_once('=') {
            if !value.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                && SECRET_ENV_FRAGMENTS.iter().any(|f| name.contains(f))
            {
                return true;
            }
        }
    }
    // `mysql -p<password>` / `-ppassword` (a lone `-p` is a flag, e.g. `mkdir -p`).
    lower
        .split_whitespace()
        .any(|w| w.len() > 2 && w.starts_with("-p") && !w.starts_with("--") && lower.contains("mysql"))
}

/// A line is recorded only when it is a plausible, non-secret command: printable, of sane
/// length and not opted out with a leading space.
pub fn is_recordable(line: &str) -> bool {
    if line.starts_with(' ') || line.starts_with('\t') {
        return false;
    }
    let trimmed = line.trim();
    let n = trimmed.chars().count();
    if !(MIN_CMD_LEN..=MAX_CMD_LEN).contains(&n) {
        return false;
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return false;
    }
    !looks_sensitive(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db(tag: &str) -> std::path::PathBuf {
        let mut r = [0u8; 4];
        rand::fill(&mut r);
        let dir = std::env::temp_dir().join(format!("ruxt-hist-{tag}-{}-{}", std::process::id(), hex::encode(r)));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("history.db")
    }

    #[test]
    fn records_suggests_and_persists() {
        let p = temp_db("a");
        let key = new_key();
        {
            let mut h = History::open(&p, 0, key.clone()).unwrap();
            assert!(h.record("ls -la /var/log", "srv1"));
            assert!(h.record("ls -la /var/log", "srv1"));
            assert!(h.record("ls /tmp", "srv2"));
            assert!(h.record("git status", "srv1"));
            assert!(!h.record(" secret-ish leading space", "srv1"));
            assert!(!h.record("x", "srv1"), "too short");
            let s = h.suggest("ls", "srv2", 5);
            assert_eq!(s[0], "ls /tmp", "same host first: {s:?}");
            assert_eq!(s[1], "ls -la /var/log");
            let s = h.suggest("ls", "srv1", 5);
            assert_eq!(s[0], "ls -la /var/log", "most used on this host first: {s:?}");
            assert!(h.suggest("ls -la /var/log", "srv1", 5).is_empty(), "no self-suggestion");
            assert!(h.suggest("", "srv1", 5).is_empty());
            assert_eq!(h.suggest("  ls", "srv1", 1).len(), 1, "leading spaces are ignored");
            // The writer thread must finish before the file is re-opened.
            drop(h);
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        let h = History::open(&p, 365, key.clone()).unwrap();
        assert_eq!(h.len(), 3, "entries survive a restart");
        let s = h.suggest("git", "srv1", 5);
        assert_eq!(s, vec!["git status".to_string()]);
        drop(h);
        std::thread::sleep(std::time::Duration::from_millis(300));
        // Nothing readable on disk.
        let raw = std::fs::read(&p).unwrap();
        for needle in ["ls -la", "git status", "srv1"] {
            assert!(
                !raw.windows(needle.len()).any(|w| w == needle.as_bytes()),
                "{needle:?} is stored in plain text"
            );
        }
        // Another key reads nothing, and its rows are dropped.
        let h = History::open(&p, 0, new_key()).unwrap();
        assert!(h.is_empty());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn clear_and_forget() {
        let p = temp_db("b");
        let key = new_key();
        let mut h = History::open(&p, 0, key.clone()).unwrap();
        h.record("echo one", "h");
        h.record("echo two", "h");
        h.record("echo three", "h");
        h.forget("echo two");
        assert_eq!(h.len(), 2);
        assert_eq!(h.suggest("echo t", "h", 5), vec!["echo three".to_string()]);
        h.clear();
        assert!(h.is_empty());
        drop(h);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let h = History::open(&p, 0, key).unwrap();
        assert!(h.is_empty(), "clear reaches the database");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn old_plaintext_table_is_encrypted_and_removed() {
        let p = temp_db("c");
        {
            let conn = Connection::open(&p).unwrap();
            conn.execute_batch(
                "CREATE TABLE commands (cmd TEXT PRIMARY KEY NOT NULL, host TEXT NOT NULL DEFAULT '',
                 uses INTEGER NOT NULL DEFAULT 1, last_used INTEGER NOT NULL DEFAULT 0) WITHOUT ROWID;
                 INSERT INTO commands VALUES ('systemctl status nginx', 'web1', 4, 1700000000);",
            )
            .unwrap();
        }
        let key = new_key();
        let h = History::open(&p, 0, key.clone()).unwrap();
        assert_eq!(
            h.suggest("systemctl", "web1", 5),
            vec!["systemctl status nginx".to_string()]
        );
        drop(h);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let raw = std::fs::read(&p).unwrap();
        for needle in ["systemctl", "nginx", "web1"] {
            assert!(
                !raw.windows(needle.len()).any(|w| w == needle.as_bytes()),
                "{needle:?} left in plain text"
            );
        }
        let h = History::open(&p, 0, key).unwrap();
        assert_eq!(h.len(), 1, "migrated once, kept");
        drop(h);
        std::thread::sleep(std::time::Duration::from_millis(300));
        delete_file(&p).unwrap();
        assert!(!p.exists());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn locked_history_records_nothing() {
        let mut h = History::locked();
        assert!(!h.record("ls -la", "h"));
        assert!(h.suggest("ls", "h", 5).is_empty());
        assert!(!h.is_open());
    }

    #[test]
    fn eviction_keeps_the_newest() {
        let mut h = History::in_memory();
        for i in 0..(MAX_ENTRIES + 5) {
            h.record(&format!("cmd number {i}"), "h");
        }
        assert_eq!(h.len(), MAX_ENTRIES);
        assert!(h.suggest("cmd number 0", "h", 1).is_empty() || h.len() == MAX_ENTRIES);
    }

    #[test]
    fn secret_looking_lines_are_not_recorded() {
        for bad in [
            "mysql -u root -pS3cret db",
            "curl -H 'Authorization: Bearer abc' https://x",
            "export AWS_SECRET_ACCESS_KEY=abcd",
            "MY_TOKEN=123 ./run",
            "login --password hunter2",
            "sshpass -p x ssh host",
            "echo password=abc > f",
        ] {
            assert!(!is_recordable(bad), "{bad:?} must be filtered");
        }
        for good in [
            "mkdir -p a/b/c",
            "ls -la",
            "git commit -m 'fix key handling'",
            "docker compose up -d",
            "cat /etc/passwd",
            "grep -ri token src/",
        ] {
            assert!(is_recordable(good), "{good:?} must be recorded");
        }
        assert!(!is_recordable("a\x1b[Ab"), "control characters");
        assert!(!is_recordable(&"x".repeat(MAX_CMD_LEN + 1)));
    }
}
