//! Port forwarding: local (-L), dynamic SOCKS5 (-D), remote (-R) bridges and agent forwarding.

use super::TabId;
use anyhow::{anyhow, bail, Context, Result};
use russh::client::{self, Handle, Msg};
use russh::Channel;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

use super::config::TunnelSpec;

/// Upper bound on simultaneous connections through one local/SOCKS listener (a local
/// process cannot exhaust the session's channels or memory).
const MAX_CONNS_PER_LISTENER: usize = 256;
/// Time a SOCKS5 client gets to finish its handshake, and the local-connect timeout for -R.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Refuse to expose a tunnel on a non-loopback interface unless the user opted in.
pub fn ensure_loopback(addr: &str) -> Result<()> {
    let a = addr.trim();
    if a.is_empty() || a == "localhost" {
        return Ok(());
    }
    match a.parse::<IpAddr>() {
        Ok(ip) if ip.is_loopback() => Ok(()),
        _ => bail!(
            "bind address {a} is not loopback; enable 'Allow public bind' in Settings to expose tunnels to the network"
        ),
    }
}

/// Parse a bind address ("" / "localhost" → 127.0.0.1; IPv6 literals accepted with or without brackets).
pub fn bind_addr(addr: &str, port: u16) -> Result<SocketAddr> {
    let a = addr.trim();
    if a.is_empty() || a == "localhost" {
        return Ok(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port));
    }
    let a = a.trim_start_matches('[').trim_end_matches(']');
    let ip: IpAddr = a
        .parse()
        .with_context(|| format!("invalid bind address {addr}:{port}"))?;
    Ok(SocketAddr::new(ip, port))
}

pub async fn bind(addr: &str, port: u16) -> Result<TcpListener> {
    Ok(TcpListener::bind(bind_addr(addr, port)?).await?)
}

/// -L: accept local TCP connections and open direct-tcpip channels to the target.
pub async fn run_local<H>(handle: Arc<Handle<H>>, listener: TcpListener, spec: TunnelSpec, tab: TabId) -> Result<()>
where
    H: client::Handler + Send + 'static,
{
    let gate = Arc::new(Semaphore::new(MAX_CONNS_PER_LISTENER));
    loop {
        let (mut sock, peer) = listener.accept().await?;
        let Ok(permit) = gate.clone().try_acquire_owned() else {
            tracing::warn!(
                "local forward {}: too many connections, refusing {peer}",
                spec.describe()
            );
            continue;
        };
        let h = handle.clone();
        let (host, port) = (spec.target_host.clone(), spec.target_port);
        tokio::spawn(async move {
            let _permit = permit;
            let ch = match h
                .channel_open_direct_tcpip(host.clone(), port as u32, peer.ip().to_string(), peer.port() as u32)
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("direct-tcpip to {host}:{port} failed: {e}");
                    return;
                }
            };
            super::audit::log(
                "forward.local.conn",
                serde_json::json!({"tab": tab, "from": peer.to_string(), "to": format!("{host}:{port}")}),
            );
            let mut stream = ch.into_stream();
            let _ = tokio::io::copy_bidirectional(&mut sock, &mut stream).await;
        });
    }
}

/// -D: a minimal, strict SOCKS5 server (CONNECT only, no auth, loopback-bound by default).
pub async fn run_socks5<H>(handle: Arc<Handle<H>>, listener: TcpListener, tab: TabId) -> Result<()>
where
    H: client::Handler + Send + 'static,
{
    let gate = Arc::new(Semaphore::new(MAX_CONNS_PER_LISTENER));
    loop {
        let (sock, peer) = listener.accept().await?;
        let Ok(permit) = gate.clone().try_acquire_owned() else {
            tracing::warn!("socks5: too many connections, refusing {peer}");
            continue;
        };
        let h = handle.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = socks5_conn(h, sock, peer, tab).await {
                tracing::debug!("socks5 {peer}: {e}");
            }
        });
    }
}

/// Parse the SOCKS5 greeting + CONNECT request; returns the target host and port.
/// Reply bytes for refusals are written to `sock` before the error is returned.
async fn socks5_handshake<S>(sock: &mut S) -> Result<(String, u16)>
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    // greeting
    let mut hdr = [0u8; 2];
    sock.read_exact(&mut hdr).await?;
    if hdr[0] != 5 {
        bail!("not SOCKS5");
    }
    let n = hdr[1] as usize;
    let mut methods = vec![0u8; n];
    sock.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        sock.write_all(&[5, 0xff]).await?;
        bail!("client requires auth");
    }
    sock.write_all(&[5, 0]).await?;

    // request
    let mut req = [0u8; 4];
    sock.read_exact(&mut req).await?;
    if req[0] != 5 {
        bail!("bad version");
    }
    if req[1] != 1 {
        sock.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?; // command not supported
        bail!("only CONNECT supported");
    }
    let host = match req[3] {
        1 => {
            let mut b = [0u8; 4];
            sock.read_exact(&mut b).await?;
            IpAddr::from(b).to_string()
        }
        3 => {
            let mut l = [0u8; 1];
            sock.read_exact(&mut l).await?;
            let mut name = vec![0u8; l[0] as usize];
            sock.read_exact(&mut name).await?;
            let s = String::from_utf8(name).map_err(|_| anyhow!("bad hostname"))?;
            if s.is_empty() || s.chars().any(|c| c.is_control() || c.is_whitespace()) {
                bail!("bad hostname");
            }
            s
        }
        4 => {
            let mut b = [0u8; 16];
            sock.read_exact(&mut b).await?;
            IpAddr::from(b).to_string()
        }
        _ => {
            sock.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            bail!("bad address type");
        }
    };
    let mut pb = [0u8; 2];
    sock.read_exact(&mut pb).await?;
    Ok((host, u16::from_be_bytes(pb)))
}

async fn socks5_conn<H>(handle: Arc<Handle<H>>, mut sock: TcpStream, peer: SocketAddr, tab: TabId) -> Result<()>
where
    H: client::Handler + Send + 'static,
{
    let (host, port) = tokio::time::timeout(HANDSHAKE_TIMEOUT, socks5_handshake(&mut sock))
        .await
        .context("SOCKS5 handshake timed out")??;

    let ch = match handle
        .channel_open_direct_tcpip(host.clone(), port as u32, peer.ip().to_string(), peer.port() as u32)
        .await
    {
        Ok(c) => c,
        Err(e) => {
            sock.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).await?; // connection refused
            bail!("direct-tcpip {host}:{port}: {e}");
        }
    };
    sock.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
    super::audit::log(
        "forward.socks.conn",
        serde_json::json!({"tab": tab, "from": peer.to_string(), "to": format!("{host}:{port}")}),
    );
    let mut stream = ch.into_stream();
    let _ = tokio::io::copy_bidirectional(&mut sock, &mut stream).await;
    Ok(())
}

/// -R: the server opened a forwarded-tcpip channel; connect it to the local target.
pub async fn bridge_to_tcp(channel: Channel<Msg>, host: String, port: u16) {
    let connect = tokio::time::timeout(HANDSHAKE_TIMEOUT, TcpStream::connect((host.as_str(), port))).await;
    match connect {
        Ok(Ok(mut sock)) => {
            let mut stream = channel.into_stream();
            let _ = tokio::io::copy_bidirectional(&mut sock, &mut stream).await;
        }
        Ok(Err(e)) => {
            tracing::warn!("remote forward target {host}:{port} unreachable: {e}");
            let _ = channel.close().await;
        }
        Err(_) => {
            tracing::warn!("remote forward target {host}:{port}: connect timed out");
            let _ = channel.close().await;
        }
    }
}

// ---------------------------------------------------------------------------
// SSH agent
// ---------------------------------------------------------------------------

#[cfg(windows)]
pub type AgentStream = tokio::net::windows::named_pipe::NamedPipeClient;
#[cfg(unix)]
pub type AgentStream = tokio::net::UnixStream;

pub type Agent = russh::keys::agent::client::AgentClient<AgentStream>;

/// Connect to the platform SSH agent, if one is running.
pub async fn connect_agent() -> Option<Agent> {
    #[cfg(windows)]
    {
        let path = std::env::var("SSH_AUTH_SOCK").unwrap_or_else(|_| r"\\.\pipe\openssh-ssh-agent".to_string());
        match Agent::connect_named_pipe(&path).await {
            Ok(a) => Some(a),
            Err(e) => {
                tracing::debug!("no ssh agent at {path}: {e}");
                None
            }
        }
    }
    #[cfg(unix)]
    {
        match Agent::connect_env().await {
            Ok(a) => Some(a),
            Err(e) => {
                tracing::debug!("no ssh agent: {e}");
                None
            }
        }
    }
}

async fn connect_agent_stream() -> Result<AgentStream> {
    #[cfg(windows)]
    {
        let path = std::env::var("SSH_AUTH_SOCK").unwrap_or_else(|_| r"\\.\pipe\openssh-ssh-agent".to_string());
        Ok(tokio::net::windows::named_pipe::ClientOptions::new().open(&path)?)
    }
    #[cfg(unix)]
    {
        let path = std::env::var("SSH_AUTH_SOCK").context("SSH_AUTH_SOCK not set")?;
        Ok(tokio::net::UnixStream::connect(path).await?)
    }
}

/// Agent forwarding: bridge an auth-agent channel from the server to the local agent.
pub async fn bridge_to_agent(channel: Channel<Msg>) -> Result<()> {
    let mut agent = tokio::time::timeout(HANDSHAKE_TIMEOUT, connect_agent_stream())
        .await
        .context("connecting to the local ssh agent timed out")??;
    let mut stream = channel.into_stream();
    tokio::io::copy_bidirectional(&mut agent, &mut stream).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_policy_and_bind_parsing() {
        assert!(ensure_loopback("").is_ok());
        assert!(ensure_loopback("localhost").is_ok());
        assert!(ensure_loopback("127.0.0.1").is_ok());
        assert!(ensure_loopback("::1").is_ok());
        assert!(ensure_loopback("0.0.0.0").is_err());
        assert!(ensure_loopback("192.168.1.5").is_err());
        assert!(ensure_loopback("example.com").is_err());
        assert_eq!(bind_addr("", 8080).unwrap(), "127.0.0.1:8080".parse().unwrap());
        assert_eq!(bind_addr("::1", 8080).unwrap(), "[::1]:8080".parse().unwrap());
        assert_eq!(bind_addr("[::1]", 8080).unwrap(), "[::1]:8080".parse().unwrap());
        assert!(bind_addr("not an ip", 1).is_err());
    }

    #[tokio::test]
    async fn socks5_parses_connect_and_rejects_other_commands() {
        // CONNECT to example.com:443 via domain name.
        let mut req = vec![5u8, 1, 0, 5, 1, 0, 3];
        req.push(11);
        req.extend_from_slice(b"example.com");
        req.extend_from_slice(&443u16.to_be_bytes());
        let (mut client, mut server) = tokio::io::duplex(256);
        client.write_all(&req).await.unwrap();
        let (host, port) = socks5_handshake(&mut server).await.unwrap();
        assert_eq!((host.as_str(), port), ("example.com", 443));
        let mut reply = [0u8; 2];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply, [5, 0]);

        // IPv4 literal
        let req = [5u8, 1, 0, 5, 1, 0, 1, 10, 0, 0, 1, 0, 80];
        let (mut client, mut server) = tokio::io::duplex(256);
        client.write_all(&req).await.unwrap();
        assert_eq!(
            socks5_handshake(&mut server).await.unwrap(),
            ("10.0.0.1".to_string(), 80)
        );

        // BIND (cmd 2) is refused with "command not supported".
        let req = [5u8, 1, 0, 5, 2, 0, 1, 10, 0, 0, 1, 0, 80];
        let (mut client, mut server) = tokio::io::duplex(256);
        client.write_all(&req).await.unwrap();
        assert!(socks5_handshake(&mut server).await.is_err());
        // Two-byte method reply, then the ten-byte request reply whose second byte is the code.
        let mut reply = [0u8; 12];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply[..2], &[5, 0]);
        assert_eq!(reply[3], 7, "reply code must be 'command not supported'");

        // Clients demanding authentication are refused.
        let (mut client, mut server) = tokio::io::duplex(256);
        client.write_all(&[5, 1, 2]).await.unwrap();
        assert!(socks5_handshake(&mut server).await.is_err());
        let mut reply = [0u8; 2];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply, [5, 0xff]);
    }
}
