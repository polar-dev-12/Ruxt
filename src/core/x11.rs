//! X11 forwarding: incoming X11 channels are connected to a local X server.
//!
//! We hand the remote side a *fake* MIT-MAGIC-COOKIE-1 cookie (so the real one never
//! travels over the wire) and swap it for the real cookie from `.Xauthority` when the
//! connection setup packet passes through — exactly what OpenSSH does. Without a real local
//! cookie the channel is refused: we never present an unauthenticated setup packet, because a
//! permissive server (`xhost +`) would then grant the remote host full display access.

use anyhow::{bail, Context, Result};
use russh::client::Msg;
use russh::Channel;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

/// Time allowed for the remote client's setup packet and for the local TCP connect.
const IO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct X11Target {
    pub host: String,
    pub port: u16,
    pub display_number: u16,
    pub real_cookie: Option<Vec<u8>>,
}

/// Authority file of an X server started by Ruxt itself (see `xserver.rs`).
static MANAGED_AUTH: parking_lot::Mutex<Option<(u16, std::path::PathBuf)>> = parking_lot::Mutex::new(None);

pub fn set_managed_auth(display: u16, path: std::path::PathBuf) {
    *MANAGED_AUTH.lock() = Some((display, path));
}

pub fn clear_managed_auth() {
    *MANAGED_AUTH.lock() = None;
}

pub fn managed_display() -> Option<u16> {
    MANAGED_AUTH.lock().as_ref().map(|(d, _)| *d)
}

/// Connect to the local X server, first waiting out the start-up of one Ruxt just launched
/// (an early connection would hang XWin; see `xserver::STARTING`).
async fn connect_local_x(target: &X11Target) -> Result<TcpStream> {
    if target.port == 6000 + target.display_number {
        super::xserver::wait_until_ready(target.display_number).await;
    }
    Ok(
        tokio::time::timeout(IO_TIMEOUT, TcpStream::connect((target.host.as_str(), target.port)))
            .await
            .context("connecting to the local X server timed out")??,
    )
}

/// Parse a DISPLAY-like string. Empty → managed server → $DISPLAY → ":0".
pub fn resolve(cfg: &str) -> X11Target {
    let mut s = cfg.trim().to_string();
    if s.is_empty() {
        s = match managed_display() {
            Some(d) => format!(":{d}"),
            None => std::env::var("DISPLAY").unwrap_or_else(|_| ":0".into()),
        };
    }
    let (host, rest) = match s.rfind(':') {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => ("", s.as_str()),
    };
    let display_number: u16 = rest.split('.').next().and_then(|n| n.parse().ok()).unwrap_or(0);
    let host = if host.is_empty() || host == "unix" {
        "127.0.0.1".to_string()
    } else {
        host.to_string()
    };
    let real_cookie = read_xauth_cookie(display_number);
    X11Target {
        host,
        port: 6000 + display_number,
        display_number,
        real_cookie,
    }
}

fn xauth_path(display: u16) -> Option<std::path::PathBuf> {
    let managed = MANAGED_AUTH
        .lock()
        .as_ref()
        .filter(|(d, _)| *d == display)
        .map(|(_, p)| p.clone());
    managed.or_else(|| {
        std::env::var_os("XAUTHORITY")
            .map(std::path::PathBuf::from)
            .or_else(|| super::hostkeys::home_dir().map(|h| h.join(".Xauthority")))
    })
}

fn read_xauth_cookie(display: u16) -> Option<Vec<u8>> {
    let data = std::fs::read(xauth_path(display)?).ok()?;
    cookie_from_xauth(&data, display)
}

/// Pick the MIT-MAGIC-COOKIE-1 entry for `display` from `.Xauthority` bytes. Entries whose
/// address is this machine (FamilyLocal with our host name, or 127.0.0.1) win over entries for
/// other hosts that happen to use the same display number.
pub fn cookie_from_xauth(data: &[u8], display: u16) -> Option<Vec<u8>> {
    let hostname = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default();
    let mut i = 0usize;
    let rd = |i: &mut usize| -> Option<Vec<u8>> {
        if *i + 2 > data.len() {
            return None;
        }
        let len = u16::from_be_bytes([data[*i], data[*i + 1]]) as usize;
        *i += 2;
        if *i + len > data.len() {
            return None;
        }
        let v = data[*i..*i + len].to_vec();
        *i += len;
        Some(v)
    };
    let want = display.to_string();
    let mut fallback: Option<Vec<u8>> = None;
    while i + 2 <= data.len() {
        let family = u16::from_be_bytes([data[i], data[i + 1]]);
        i += 2;
        let addr = rd(&mut i)?;
        let number = rd(&mut i)?;
        let name = rd(&mut i)?;
        let cookie = rd(&mut i)?;
        if name != b"MIT-MAGIC-COOKIE-1" || number != want.as_bytes() {
            continue;
        }
        let local = match family {
            256 => hostname.is_empty() || addr == hostname.as_bytes() || addr.is_empty(),
            0 => addr == [127, 0, 0, 1],
            6 => addr == [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            _ => false,
        };
        if local {
            return Some(cookie);
        }
        if fallback.is_none() {
            fallback = Some(cookie);
        }
    }
    fallback
}

/// Constant-time byte comparison (the cookie check must not leak timing to the remote).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Bridge one X11 channel to the local X server, rewriting the auth cookie.
pub async fn bridge(channel: Channel<Msg>, target: X11Target, fake_cookie: [u8; 16]) -> Result<()> {
    bridge_io(channel.into_stream(), target, fake_cookie).await
}

/// Transport-agnostic body of [`bridge`] (unit-tested with an in-memory duplex stream).
pub async fn bridge_io<S>(mut stream: S, target: X11Target, fake_cookie: [u8; 16]) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let Some(real) = target.real_cookie.as_deref() else {
        bail!(
            "no local X authority cookie for display :{}; refusing to connect the remote client without authentication",
            target.display_number
        );
    };

    // X11 connection setup: 12-byte header then padded auth name + data.
    let mut hdr = [0u8; 12];
    tokio::time::timeout(IO_TIMEOUT, stream.read_exact(&mut hdr))
        .await
        .context("X11 setup packet timed out")??;
    let le = match hdr[0] {
        b'l' => true,
        b'B' => false,
        _ => bail!("not an X11 setup packet"),
    };
    let u16_at = |b: &[u8], i: usize| -> u16 {
        if le {
            u16::from_le_bytes([b[i], b[i + 1]])
        } else {
            u16::from_be_bytes([b[i], b[i + 1]])
        }
    };
    let n_name = u16_at(&hdr, 6) as usize;
    let n_data = u16_at(&hdr, 8) as usize;
    if n_name > 256 || n_data > 1024 {
        bail!("absurd X11 auth lengths");
    }
    let pad = |n: usize| (4 - (n % 4)) % 4;
    let mut name = vec![0u8; n_name + pad(n_name)];
    let mut data = vec![0u8; n_data + pad(n_data)];
    tokio::time::timeout(IO_TIMEOUT, async {
        stream.read_exact(&mut name).await?;
        stream.read_exact(&mut data).await
    })
    .await
    .context("X11 setup packet timed out")??;

    if &name[..n_name] != b"MIT-MAGIC-COOKIE-1" || !ct_eq(&data[..n_data], &fake_cookie) {
        // Unknown/foreign cookie: do not pass it through; the remote is misbehaving.
        bail!("X11 client presented an unexpected auth cookie");
    }

    let mut out = Vec::with_capacity(12 + name.len() + real.len() + 4);
    let mut h = hdr;
    let len = real.len() as u16;
    let lb = if le { len.to_le_bytes() } else { len.to_be_bytes() };
    h[8] = lb[0];
    h[9] = lb[1];
    out.extend_from_slice(&h);
    out.extend_from_slice(&name);
    out.extend_from_slice(real);
    out.extend(std::iter::repeat_n(0u8, pad(real.len())));

    let mut sock = connect_local_x(&target).await?;
    sock.write_all(&out).await?;
    let _ = tokio::io::copy_bidirectional(&mut sock, &mut stream).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Untrusted ("ssh -X") authorisations via the X SECURITY extension
// ---------------------------------------------------------------------------
//
// X11 trusts every client of a display equally: a forwarded program may read other windows,
// grab the keyboard or synthesise input. The SECURITY extension lets a trusted client mint a
// second MIT-MAGIC-COOKIE-1 authorisation with the *untrusted* trust level; clients holding
// that cookie are confined (no access to other clients' resources, no input snooping). This
// is what `xauth generate … untrusted` does for OpenSSH's default `ForwardX11Trusted no`.
//
// Only the handful of core/SECURITY requests needed are implemented, little-endian, over the
// same TCP connection the bridge uses.

const X_PROTO: &[u8] = b"MIT-MAGIC-COOKIE-1";
const X_QUERY_EXTENSION: u8 = 98;
const X_GET_INPUT_FOCUS: u8 = 43;
const SEC_GENERATE_AUTHORIZATION: u8 = 1;
const SEC_REVOKE_AUTHORIZATION: u8 = 2;
const SEC_MASK_TIMEOUT: u32 = 1 << 0;
const SEC_MASK_TRUST_LEVEL: u32 = 1 << 1;
const SEC_TRUST_UNTRUSTED: u32 = 1;
/// Largest reply body accepted from the X server.
const MAX_REPLY: usize = 1 << 20;

/// An untrusted authorisation minted on the local X server.
#[derive(Debug, Clone)]
pub struct UntrustedAuth {
    /// The cookie forwarded clients must present (handed to the bridge as `real_cookie`).
    pub cookie: Vec<u8>,
    /// Server-side id, needed to revoke the authorisation when the session ends.
    pub auth_id: u32,
}

fn pad4(n: usize) -> usize {
    (4 - (n % 4)) % 4
}

/// Connection setup with the master cookie; errors carry the server's reason.
async fn x_setup<S>(s: &mut S, cookie: &[u8]) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut req = vec![b'l', 0, 11, 0, 0, 0];
    req.extend_from_slice(&(X_PROTO.len() as u16).to_le_bytes());
    req.extend_from_slice(&(cookie.len() as u16).to_le_bytes());
    req.extend_from_slice(&[0, 0]);
    req.extend_from_slice(X_PROTO);
    req.extend(std::iter::repeat_n(0u8, pad4(X_PROTO.len())));
    req.extend_from_slice(cookie);
    req.extend(std::iter::repeat_n(0u8, pad4(cookie.len())));
    s.write_all(&req).await?;
    let mut hdr = [0u8; 8];
    s.read_exact(&mut hdr).await?;
    let extra = u16::from_le_bytes([hdr[6], hdr[7]]) as usize * 4;
    if extra > MAX_REPLY {
        bail!("X setup reply too large");
    }
    let mut rest = vec![0u8; extra];
    s.read_exact(&mut rest).await?;
    match hdr[0] {
        1 => Ok(()),
        0 => {
            let n = (hdr[1] as usize).min(rest.len());
            bail!(
                "X server refused the connection: {}",
                super::term::sanitize_title(&String::from_utf8_lossy(&rest[..n]))
            )
        }
        2 => bail!("X server demands further authentication"),
        other => bail!("unexpected X setup status {other}"),
    }
}

/// Read one reply (32-byte head plus body), skipping events and turning X errors into `Err`.
async fn x_reply<S>(s: &mut S) -> Result<Vec<u8>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        let mut head = [0u8; 32];
        s.read_exact(&mut head).await?;
        match head[0] {
            0 => bail!(
                "X error {} (request {}.{})",
                head[1],
                head[10],
                u16::from_le_bytes([head[8], head[9]])
            ),
            1 => {
                let extra = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
                let extra = extra
                    .checked_mul(4)
                    .ok_or_else(|| anyhow::anyhow!("X reply length overflow"))?;
                if extra > MAX_REPLY {
                    bail!("X reply too large");
                }
                let mut v = head.to_vec();
                v.resize(32 + extra, 0);
                s.read_exact(&mut v[32..]).await?;
                return Ok(v);
            }
            _ => continue, // an event; none were selected, ignore defensively
        }
    }
}

/// Major opcode of `name`, if the server has the extension.
async fn x_query_extension<S>(s: &mut S, name: &[u8]) -> Result<Option<u8>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let padded = name.len() + pad4(name.len());
    let mut req = vec![X_QUERY_EXTENSION, 0];
    req.extend_from_slice(&((2 + padded / 4) as u16).to_le_bytes());
    req.extend_from_slice(&(name.len() as u16).to_le_bytes());
    req.extend_from_slice(&[0, 0]);
    req.extend_from_slice(name);
    req.extend(std::iter::repeat_n(0u8, pad4(name.len())));
    s.write_all(&req).await?;
    let r = x_reply(s).await?;
    Ok((r[8] != 0).then_some(r[9]))
}

/// Round trip that guarantees earlier requests were processed.
async fn x_sync<S>(s: &mut S) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    s.write_all(&[X_GET_INPUT_FOCUS, 0, 1, 0]).await?;
    x_reply(s).await.map(|_| ())
}

/// Ask the SECURITY extension for an untrusted MIT-MAGIC-COOKIE-1 authorisation that stays
/// valid `timeout_secs` seconds after its last client disconnects.
async fn security_generate<S>(s: &mut S, major: u8, timeout_secs: u32) -> Result<UntrustedAuth>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let proto_padded = X_PROTO.len() + pad4(X_PROTO.len());
    let words = 3 + proto_padded / 4 + 2;
    let mut req = vec![major, SEC_GENERATE_AUTHORIZATION];
    req.extend_from_slice(&(words as u16).to_le_bytes());
    req.extend_from_slice(&(X_PROTO.len() as u16).to_le_bytes());
    req.extend_from_slice(&0u16.to_le_bytes()); // empty auth data: the server mints a cookie
    req.extend_from_slice(&(SEC_MASK_TIMEOUT | SEC_MASK_TRUST_LEVEL).to_le_bytes());
    req.extend_from_slice(X_PROTO);
    req.extend(std::iter::repeat_n(0u8, pad4(X_PROTO.len())));
    req.extend_from_slice(&timeout_secs.to_le_bytes());
    req.extend_from_slice(&SEC_TRUST_UNTRUSTED.to_le_bytes());
    s.write_all(&req).await?;
    let r = x_reply(s).await?;
    let auth_id = u32::from_le_bytes([r[8], r[9], r[10], r[11]]);
    let n = u16::from_le_bytes([r[12], r[13]]) as usize;
    if n == 0 || n > 64 || 32 + n > r.len() {
        bail!("X server returned no usable untrusted cookie");
    }
    Ok(UntrustedAuth {
        cookie: r[32..32 + n].to_vec(),
        auth_id,
    })
}

async fn security_revoke<S>(s: &mut S, major: u8, auth_id: u32) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut req = vec![major, SEC_REVOKE_AUTHORIZATION, 2, 0];
    req.extend_from_slice(&auth_id.to_le_bytes());
    s.write_all(&req).await?;
    x_sync(s).await
}

/// Mint an untrusted authorisation on `target`'s X server (transport-agnostic body of
/// [`generate_untrusted`]).
pub async fn generate_untrusted_io<S>(stream: &mut S, master_cookie: &[u8], timeout_secs: u32) -> Result<UntrustedAuth>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    x_setup(stream, master_cookie).await?;
    let Some(major) = x_query_extension(stream, b"SECURITY").await? else {
        bail!("the X server has no SECURITY extension (needed for untrusted X11 forwarding)");
    };
    security_generate(stream, major, timeout_secs).await
}

pub async fn revoke_untrusted_io<S>(stream: &mut S, master_cookie: &[u8], auth_id: u32) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    x_setup(stream, master_cookie).await?;
    let Some(major) = x_query_extension(stream, b"SECURITY").await? else {
        bail!("no SECURITY extension");
    };
    security_revoke(stream, major, auth_id).await
}

/// How long an untrusted authorisation stays valid after its last client disconnects (and
/// after creation while no client has used it). Not `u32::MAX`: the X server's timer
/// arithmetic wraps for anything beyond ~49 days and XWin 21.1 then crashes.
pub const UNTRUSTED_TIMEOUT_SECS: u32 = 7 * 24 * 3600;

/// Connect to the local X server with the master cookie and mint an untrusted cookie for
/// forwarded clients. `timeout_secs = 0` selects [`UNTRUSTED_TIMEOUT_SECS`]; the
/// authorisation is revoked with [`revoke_untrusted`] when the session ends.
pub async fn generate_untrusted(target: &X11Target, master_cookie: &[u8], timeout_secs: u32) -> Result<UntrustedAuth> {
    let mut sock = connect_local_x(target).await?;
    let timeout = if timeout_secs == 0 {
        UNTRUSTED_TIMEOUT_SECS
    } else {
        timeout_secs.min(UNTRUSTED_TIMEOUT_SECS)
    };
    tokio::time::timeout(IO_TIMEOUT, generate_untrusted_io(&mut sock, master_cookie, timeout))
        .await
        .context("X server did not answer the SECURITY request in time")?
}

pub async fn revoke_untrusted(target: &X11Target, master_cookie: &[u8], auth_id: u32) -> Result<()> {
    let mut sock = connect_local_x(target).await?;
    tokio::time::timeout(IO_TIMEOUT, revoke_untrusted_io(&mut sock, master_cookie, auth_id))
        .await
        .context("X server did not answer the revoke request in time")?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::net::TcpListener;

    /// Minimal X server: accepts the setup with `expect_cookie`, answers QueryExtension
    /// (SECURITY present at opcode 200 unless `no_security`), GenerateAuthorization and
    /// GetInputFocus; records revoked ids.
    async fn fake_xserver<S>(
        mut s: S,
        expect_cookie: Vec<u8>,
        no_security: bool,
        revoked: Arc<parking_lot::Mutex<Vec<u32>>>,
    ) where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let mut hdr = [0u8; 12];
        s.read_exact(&mut hdr).await.unwrap();
        let n_name = u16::from_le_bytes([hdr[6], hdr[7]]) as usize;
        let n_data = u16::from_le_bytes([hdr[8], hdr[9]]) as usize;
        let mut body = vec![0u8; n_name + pad4(n_name) + n_data + pad4(n_data)];
        s.read_exact(&mut body).await.unwrap();
        let data = &body[n_name + pad4(n_name)..n_name + pad4(n_name) + n_data];
        if data != expect_cookie {
            let reason = b"bad cookie";
            let mut rep = vec![0u8, reason.len() as u8, 11, 0, 0, 0, 3, 0];
            rep.extend_from_slice(reason);
            rep.extend(std::iter::repeat_n(0u8, 2));
            s.write_all(&rep).await.unwrap();
            return;
        }
        s.write_all(&[1u8, 0, 11, 0, 0, 0, 0, 0]).await.unwrap();
        let mut seq: u16 = 0;
        loop {
            let mut rh = [0u8; 4];
            if s.read_exact(&mut rh).await.is_err() {
                return;
            }
            seq = seq.wrapping_add(1);
            let len = u16::from_le_bytes([rh[2], rh[3]]) as usize * 4;
            let mut rest = vec![0u8; len.saturating_sub(4)];
            s.read_exact(&mut rest).await.unwrap();
            match (rh[0], rh[1]) {
                (X_QUERY_EXTENSION, _) => {
                    let n = u16::from_le_bytes([rest[0], rest[1]]) as usize;
                    let name = &rest[4..4 + n];
                    let present = name == b"SECURITY" && !no_security;
                    let mut rep = vec![1u8, 0];
                    rep.extend_from_slice(&seq.to_le_bytes());
                    rep.extend_from_slice(&0u32.to_le_bytes());
                    rep.push(present as u8);
                    rep.push(200);
                    rep.extend(std::iter::repeat_n(0u8, 22));
                    s.write_all(&rep).await.unwrap();
                }
                (200, SEC_GENERATE_AUTHORIZATION) => {
                    let n_proto = u16::from_le_bytes([rest[0], rest[1]]) as usize;
                    let mask = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]);
                    assert_eq!(&rest[8..8 + n_proto], X_PROTO);
                    assert_eq!(mask, SEC_MASK_TIMEOUT | SEC_MASK_TRUST_LEVEL);
                    let vals = &rest[8 + n_proto + pad4(n_proto)..];
                    let trust = u32::from_le_bytes([vals[4], vals[5], vals[6], vals[7]]);
                    assert_eq!(trust, SEC_TRUST_UNTRUSTED);
                    let cookie = [0x5au8; 16];
                    let mut rep = vec![1u8, 0];
                    rep.extend_from_slice(&seq.to_le_bytes());
                    rep.extend_from_slice(&4u32.to_le_bytes()); // 16 bytes of data
                    rep.extend_from_slice(&0x1234u32.to_le_bytes());
                    rep.extend_from_slice(&16u16.to_le_bytes());
                    rep.extend(std::iter::repeat_n(0u8, 18));
                    rep.extend_from_slice(&cookie);
                    s.write_all(&rep).await.unwrap();
                }
                (200, SEC_REVOKE_AUTHORIZATION) => {
                    revoked
                        .lock()
                        .push(u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]));
                }
                (X_GET_INPUT_FOCUS, _) => {
                    let mut rep = vec![1u8, 0];
                    rep.extend_from_slice(&seq.to_le_bytes());
                    rep.extend_from_slice(&0u32.to_le_bytes());
                    rep.extend(std::iter::repeat_n(0u8, 24));
                    s.write_all(&rep).await.unwrap();
                }
                other => panic!("unexpected request {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn untrusted_cookie_is_generated_and_revoked() {
        let master = vec![0xabu8; 16];
        let revoked = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let (mut client, server) = tokio::io::duplex(4096);
        let srv = tokio::spawn(fake_xserver(server, master.clone(), false, revoked.clone()));
        let auth = generate_untrusted_io(&mut client, &master, 0).await.unwrap();
        assert_eq!(auth.cookie, vec![0x5au8; 16]);
        assert_eq!(auth.auth_id, 0x1234);
        drop(client);
        let _ = srv.await;
        // Revocation happens on a fresh connection (the session is ending).
        let (mut client, server) = tokio::io::duplex(4096);
        let srv = tokio::spawn(fake_xserver(server, master.clone(), false, revoked.clone()));
        revoke_untrusted_io(&mut client, &master, auth.auth_id).await.unwrap();
        assert_eq!(*revoked.lock(), vec![0x1234]);
        drop(client);
        let _ = srv.await;
    }

    #[tokio::test]
    async fn untrusted_fails_cleanly_without_security_or_with_wrong_cookie() {
        let master = vec![0xabu8; 16];
        let revoked = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let (mut client, server) = tokio::io::duplex(4096);
        let srv = tokio::spawn(fake_xserver(server, master.clone(), true, revoked.clone()));
        let err = generate_untrusted_io(&mut client, &master, 0)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("SECURITY"), "{err}");
        drop(client);
        let _ = srv.await;

        let (mut client, server) = tokio::io::duplex(4096);
        let srv = tokio::spawn(fake_xserver(server, master.clone(), false, revoked));
        let err = generate_untrusted_io(&mut client, b"wrong", 0)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("refused") && err.contains("bad cookie"), "{err}");
        drop(client);
        let _ = srv.await;
    }

    fn setup_packet(cookie: &[u8]) -> Vec<u8> {
        let name = b"MIT-MAGIC-COOKIE-1";
        let mut p = vec![b'l', 0, 11, 0, 0, 0];
        p.extend_from_slice(&(name.len() as u16).to_le_bytes());
        p.extend_from_slice(&(cookie.len() as u16).to_le_bytes());
        p.extend_from_slice(&[0, 0]);
        p.extend_from_slice(name);
        p.extend(std::iter::repeat_n(0u8, (4 - name.len() % 4) % 4));
        p.extend_from_slice(cookie);
        p.extend(std::iter::repeat_n(0u8, (4 - cookie.len() % 4) % 4));
        p
    }

    #[tokio::test]
    async fn bridge_swaps_fake_cookie_for_real_one() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let real = vec![0xabu8; 16];
        let fake = [0x11u8; 16];
        let target = X11Target {
            host: "127.0.0.1".into(),
            port,
            display_number: 0,
            real_cookie: Some(real.clone()),
        };
        let (mut client, server_side) = tokio::io::duplex(4096);
        let bridge_task = tokio::spawn(bridge_io(server_side, target, fake));
        client.write_all(&setup_packet(&fake)).await.unwrap();
        let (mut xs, _) = listener.accept().await.unwrap();
        let mut got = vec![0u8; 12 + 20 + 16];
        xs.read_exact(&mut got).await.unwrap();
        assert_eq!(&got[..12], &setup_packet(&real)[..12]);
        assert_eq!(&got[32..48], &real[..]);
        drop(xs);
        drop(client);
        let _ = bridge_task.await;
    }

    #[tokio::test]
    async fn bridge_refuses_wrong_cookie_and_missing_local_cookie() {
        let fake = [0x11u8; 16];
        let target = X11Target {
            host: "127.0.0.1".into(),
            port: 1,
            display_number: 0,
            real_cookie: Some(vec![1; 16]),
        };
        let (mut client, server_side) = tokio::io::duplex(4096);
        let t = tokio::spawn(bridge_io(server_side, target, fake));
        client.write_all(&setup_packet(&[0x22u8; 16])).await.unwrap();
        let err = t.await.unwrap().unwrap_err().to_string();
        assert!(err.contains("unexpected auth cookie"), "{err}");

        let target = X11Target {
            host: "127.0.0.1".into(),
            port: 1,
            display_number: 3,
            real_cookie: None,
        };
        let (_client, server_side) = tokio::io::duplex(64);
        let err = bridge_io(server_side, target, fake).await.unwrap_err().to_string();
        assert!(err.contains("no local X authority cookie"), "{err}");
    }

    #[test]
    fn xauth_prefers_local_entries() {
        fn entry(out: &mut Vec<u8>, family: u16, addr: &[u8], num: &str, cookie: &[u8]) {
            out.extend_from_slice(&family.to_be_bytes());
            for f in [addr, num.as_bytes(), b"MIT-MAGIC-COOKIE-1", cookie] {
                out.extend_from_slice(&(f.len() as u16).to_be_bytes());
                out.extend_from_slice(f);
            }
        }
        let mut data = Vec::new();
        entry(&mut data, 0, &[10, 0, 0, 7], "0", &[0xaa; 16]); // some other host, display 0
        entry(&mut data, 0, &[127, 0, 0, 1], "0", &[0xbb; 16]); // local, display 0
        entry(&mut data, 0, &[127, 0, 0, 1], "1", &[0xcc; 16]);
        assert_eq!(cookie_from_xauth(&data, 0).as_deref(), Some(&[0xbb; 16][..]));
        assert_eq!(cookie_from_xauth(&data, 1).as_deref(), Some(&[0xcc; 16][..]));
        assert_eq!(cookie_from_xauth(&data, 2), None);
        assert!(ct_eq(b"abc", b"abc") && !ct_eq(b"abc", b"abd") && !ct_eq(b"ab", b"abc"));
    }
}
