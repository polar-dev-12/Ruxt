//! Server status monitor.
//!
//! Every few seconds a short, read-only shell script is run on the server over its own
//! `exec` channel of the existing SSH session (no second login, no extra password). The
//! output is parsed here into a [`Sample`]: host name, OS, uptime, load, CPU use (from two
//! consecutive `/proc/stat` readings), memory, swap, disks, logged-in users, process
//! count and network download/upload rates (from two `/proc/net/dev` readings). Fields a
//! server does not provide (no `/proc` on macOS/BSD) stay `None`.
//!
//! The server is untrusted (threat model T2): the script's output is size-capped, every
//! number is parsed defensively and every string is run through `sanitize_title`.

use super::ssh::{ClientHandler, UiEvent};
use super::term::sanitize_title;
use super::TabId;
use anyhow::{bail, Result};
use russh::client::Handle;
use russh::ChannelMsg;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Allowed polling interval (seconds) and the default.
pub const INTERVAL_RANGE: std::ops::RangeInclusive<u64> = 2..=120;
pub const DEFAULT_INTERVAL: u64 = 5;
/// Output beyond this is discarded (a hostile server could stream forever).
const MAX_OUTPUT: usize = 256 * 1024;
/// One sample (channel open, exec, output) must complete within this.
const SAMPLE_TIMEOUT: Duration = Duration::from_secs(15);
/// Consecutive failures after which monitoring is switched off for the session.
const MAX_FAILURES: u32 = 3;
/// Longest string kept from the server (host name, OS name, mount point).
const MAX_FIELD: usize = 96;
/// Disks listed at most.
const MAX_DISKS: usize = 12;

/// The script. POSIX `sh` syntax only; every command is optional (`2>/dev/null`) so the
/// script succeeds on any Unix. Sections are delimited by `@@NAME` lines.
pub const SCRIPT: &str = "echo @@HOST; hostname 2>/dev/null; \
echo @@UNAME; uname -srm 2>/dev/null; \
echo @@OSREL; head -n 20 /etc/os-release 2>/dev/null; \
echo @@UPTIME; cat /proc/uptime 2>/dev/null; \
echo @@LOAD; cat /proc/loadavg 2>/dev/null; \
echo @@UPCMD; uptime 2>/dev/null; \
echo @@STAT; head -n 1 /proc/stat 2>/dev/null; \
echo @@NCPU; nproc 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null; \
echo @@MEM; head -n 40 /proc/meminfo 2>/dev/null; \
echo @@DF; LC_ALL=C df -Pk 2>/dev/null | head -n 40; \
echo @@WHO; who 2>/dev/null | wc -l; \
echo @@PS; ps -e 2>/dev/null | wc -l; \
echo @@NET; head -n 66 /proc/net/dev 2>/dev/null; \
echo @@END";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuTimes {
    pub total: u64,
    pub idle: u64,
}

/// Byte counters of all physical interfaces, summed, read at `at_secs` (server uptime).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NetCounters {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub at_secs: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Disk {
    pub mount: String,
    pub total_kb: u64,
    pub used_kb: u64,
}

impl Disk {
    pub fn used_pct(&self) -> f32 {
        if self.total_kb == 0 {
            0.0
        } else {
            (self.used_kb as f64 / self.total_kb as f64 * 100.0) as f32
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sample {
    pub hostname: String,
    pub os: String,
    pub kernel: String,
    pub uptime_secs: Option<u64>,
    pub load: Option<[f32; 3]>,
    pub ncpu: Option<u32>,
    /// Raw counters of this sample (kept so the next sample can compute a delta).
    pub cpu: Option<CpuTimes>,
    /// CPU busy percentage since the previous sample (needs two samples).
    pub cpu_pct: Option<f32>,
    pub mem_total_kb: Option<u64>,
    pub mem_avail_kb: Option<u64>,
    pub swap_total_kb: Option<u64>,
    pub swap_free_kb: Option<u64>,
    pub disks: Vec<Disk>,
    pub users: Option<u32>,
    pub procs: Option<u32>,
    /// Raw network counters of this sample (kept for the next sample's delta).
    pub net: Option<NetCounters>,
    /// Download / upload rate in bytes per second since the previous sample.
    pub net_rx_rate: Option<f64>,
    pub net_tx_rate: Option<f64>,
}

impl Sample {
    pub fn mem_used_pct(&self) -> Option<f32> {
        match (self.mem_total_kb, self.mem_avail_kb) {
            (Some(t), Some(a)) if t > 0 => Some(((t.saturating_sub(a)) as f64 / t as f64 * 100.0) as f32),
            _ => None,
        }
    }
    pub fn swap_used_pct(&self) -> Option<f32> {
        match (self.swap_total_kb, self.swap_free_kb) {
            (Some(t), Some(f)) if t > 0 => Some(((t.saturating_sub(f)) as f64 / t as f64 * 100.0) as f32),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum MonitorEvent {
    Sample(Box<Sample>),
    /// One poll failed; polling continues.
    Error(String),
    /// Polling stopped for this session (exec refused, repeated failures).
    Unavailable(String),
}

/// Shared switches the UI flips while the session runs.
#[derive(Clone)]
pub struct MonitorControl {
    pub enabled: Arc<AtomicBool>,
    pub interval_secs: Arc<AtomicU64>,
}

impl MonitorControl {
    pub fn new(enabled: bool, interval_secs: u64) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(enabled)),
            interval_secs: Arc::new(AtomicU64::new(
                interval_secs.clamp(*INTERVAL_RANGE.start(), *INTERVAL_RANGE.end()),
            )),
        }
    }
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }
    pub fn set_interval(&self, secs: u64) {
        self.interval_secs.store(
            secs.clamp(*INTERVAL_RANGE.start(), *INTERVAL_RANGE.end()),
            Ordering::Relaxed,
        );
    }
    pub fn interval_secs(&self) -> u64 {
        self.interval_secs.load(Ordering::Relaxed)
    }
    fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_secs.load(Ordering::Relaxed))
    }
}

/// Polling loop; runs until aborted by the session.
pub async fn run(
    handle: Arc<Handle<ClientHandler>>,
    tab: TabId,
    ui: mpsc::UnboundedSender<UiEvent>,
    ctx: egui::Context,
    control: MonitorControl,
) {
    let send = |ev: MonitorEvent| {
        let _ = ui.send(UiEvent::Monitor { tab, ev });
        ctx.request_repaint();
    };
    let mut prev: Option<Sample> = None;
    let mut failures = 0u32;
    loop {
        if !control.is_enabled() {
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        }
        match tokio::time::timeout(SAMPLE_TIMEOUT, sample(&handle)).await {
            Ok(Ok(out)) => {
                failures = 0;
                let s = parse(&out, prev.as_ref());
                let first = !prev.as_ref().is_some_and(|p| p.cpu.is_some()) && s.cpu.is_some();
                prev = Some(s.clone());
                send(MonitorEvent::Sample(Box::new(s)));
                if first {
                    // A second reading one second later gives the CPU percentage right away.
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            }
            Ok(Err(e)) => {
                failures += 1;
                let refused = e.to_string().contains("refused");
                if refused || failures >= MAX_FAILURES {
                    control.set_enabled(false);
                    send(MonitorEvent::Unavailable(format!("{e:#}")));
                    continue;
                }
                send(MonitorEvent::Error(format!("{e:#}")));
            }
            Err(_) => {
                failures += 1;
                if failures >= MAX_FAILURES {
                    control.set_enabled(false);
                    send(MonitorEvent::Unavailable("sampling timed out".into()));
                    continue;
                }
                send(MonitorEvent::Error("sampling timed out".into()));
            }
        }
        tokio::time::sleep(control.interval()).await;
    }
}

/// Run the script once and return its (capped) standard output.
pub async fn sample(handle: &Handle<ClientHandler>) -> Result<String> {
    let mut ch = handle.channel_open_session().await?;
    ch.exec(true, SCRIPT).await?;
    let mut out: Vec<u8> = Vec::new();
    loop {
        match ch.wait().await {
            Some(ChannelMsg::Success) => {}
            Some(ChannelMsg::Failure) => bail!("server refused the exec request"),
            Some(ChannelMsg::Data { data }) => {
                let room = MAX_OUTPUT.saturating_sub(out.len());
                out.extend_from_slice(&data[..data.len().min(room)]);
                if room == 0 {
                    break;
                }
            }
            Some(ChannelMsg::Close) | None => break,
            Some(_) => {}
        }
    }
    let _ = ch.close().await;
    if out.is_empty() {
        bail!("no output from the server");
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

fn clean(s: &str) -> String {
    sanitize_title(s.trim()).chars().take(MAX_FIELD).collect()
}

fn first_line<'a>(sec: &'a [&'a str]) -> Option<&'a str> {
    sec.iter().map(|l| l.trim()).find(|l| !l.is_empty())
}

fn parse_u64(s: &str) -> Option<u64> {
    s.trim().parse().ok()
}

/// Parse the script output. `prev` is the previous sample (its raw counters give the
/// CPU percentage and network rates).
pub fn parse(output: &str, prev: Option<&Sample>) -> Sample {
    let mut sections: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut current: Option<&str> = None;
    for line in output.lines() {
        if let Some(name) = line.trim().strip_prefix("@@") {
            current = Some(name.trim());
            sections.entry(current.unwrap()).or_default();
        } else if let Some(c) = current {
            sections.entry(c).or_default().push(line);
        }
    }
    let sec = |name: &str| -> Vec<&str> { sections.get(name).cloned().unwrap_or_default() };
    let mut s = Sample::default();

    if let Some(h) = first_line(&sec("HOST")) {
        s.hostname = clean(h);
    }
    if let Some(u) = first_line(&sec("UNAME")) {
        s.kernel = clean(u);
    }
    for line in sec("OSREL") {
        if let Some(v) = line.trim().strip_prefix("PRETTY_NAME=") {
            s.os = clean(v.trim_matches('"'));
        }
    }
    let mut uptime_exact: Option<f64> = None;
    if let Some(u) = first_line(&sec("UPTIME")) {
        uptime_exact = u
            .split_whitespace()
            .next()
            .and_then(|f| f.parse::<f64>().ok())
            .filter(|f| f.is_finite() && *f >= 0.0);
        s.uptime_secs = uptime_exact.map(|f| f as u64);
    }
    if let Some(l) = first_line(&sec("LOAD")) {
        s.load = parse_load(l);
    }
    if s.load.is_none() || s.uptime_secs.is_none() {
        if let Some(u) = first_line(&sec("UPCMD")) {
            if s.load.is_none() {
                if let Some(i) = u.find("load average") {
                    let rest = &u[i..];
                    let rest = rest.split_once(':').map(|(_, r)| r).unwrap_or(rest);
                    s.load = parse_load(rest);
                }
            }
        }
    }
    if let Some(st) = first_line(&sec("STAT")) {
        if let Some(cpu) = parse_stat(st) {
            s.cpu = Some(cpu);
            if let Some(p) = prev.and_then(|p| p.cpu.as_ref()) {
                let dt = cpu.total.saturating_sub(p.total);
                let di = cpu.idle.saturating_sub(p.idle);
                if dt > 0 && di <= dt {
                    s.cpu_pct = Some(((dt - di) as f64 / dt as f64 * 100.0) as f32);
                }
            }
        }
    }
    if let Some(n) = first_line(&sec("NCPU")) {
        s.ncpu = n.parse::<u32>().ok().filter(|n| (1..=4096).contains(n));
    }
    for line in sec("MEM") {
        let Some((k, v)) = line.split_once(':') else { continue };
        let kb = v.split_whitespace().next().and_then(parse_u64);
        match k.trim() {
            "MemTotal" => s.mem_total_kb = kb,
            "MemAvailable" => s.mem_avail_kb = kb,
            "SwapTotal" => s.swap_total_kb = kb,
            "SwapFree" => s.swap_free_kb = kb,
            _ => {}
        }
    }
    if s.mem_avail_kb.is_none() && s.mem_total_kb.is_some() {
        // Old kernels: MemFree + Buffers + Cached.
        let mut sum = 0u64;
        let mut seen = false;
        for line in sec("MEM") {
            let Some((k, v)) = line.split_once(':') else { continue };
            if matches!(k.trim(), "MemFree" | "Buffers" | "Cached") {
                if let Some(kb) = v.split_whitespace().next().and_then(parse_u64) {
                    sum = sum.saturating_add(kb);
                    seen = true;
                }
            }
        }
        if seen {
            s.mem_avail_kb = Some(sum);
        }
    }
    for line in sec("DF").iter().skip(1) {
        if s.disks.len() >= MAX_DISKS {
            break;
        }
        if let Some(d) = parse_df_line(line) {
            s.disks.push(d);
        }
    }
    if let Some(w) = first_line(&sec("WHO")) {
        s.users = w.parse::<u32>().ok().filter(|n| *n < 100_000);
    }
    if let Some(p) = first_line(&sec("PS")) {
        s.procs = p
            .parse::<u32>()
            .ok()
            .filter(|n| *n < 10_000_000)
            .map(|n| n.saturating_sub(1)); // header line
    }
    if let (Some(at), Some((rx, tx))) = (uptime_exact, parse_net_dev(&sec("NET"))) {
        let cur = NetCounters {
            rx_bytes: rx,
            tx_bytes: tx,
            at_secs: at,
        };
        s.net = Some(cur);
        if let Some(p) = prev.and_then(|p| p.net) {
            let dt = cur.at_secs - p.at_secs;
            // Counters going backwards (reboot, interface reset) give no rate.
            if dt > 0.0 && cur.rx_bytes >= p.rx_bytes && cur.tx_bytes >= p.tx_bytes {
                s.net_rx_rate = Some((cur.rx_bytes - p.rx_bytes) as f64 / dt);
                s.net_tx_rate = Some((cur.tx_bytes - p.tx_bytes) as f64 / dt);
            }
        }
    }
    s
}

/// Interfaces left out of the network total: loopback and the virtual links of
/// containers / VMs (their traffic is already counted on the physical interface).
const SKIP_NET_PREFIX: &[&str] = &[
    "lo", "veth", "docker", "br-", "virbr", "cni", "flannel", "cali", "tun", "tap",
];

/// Sum received / transmitted bytes over `/proc/net/dev`.
fn parse_net_dev(lines: &[&str]) -> Option<(u64, u64)> {
    let mut total: Option<(u64, u64)> = None;
    for line in lines {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || SKIP_NET_PREFIX.iter().any(|p| name.starts_with(p)) {
            continue;
        }
        let f: Vec<&str> = rest.split_whitespace().collect();
        // Receive: bytes packets errs drop fifo frame compressed multicast, then transmit.
        let (Some(rx), Some(tx)) = (
            f.first().and_then(|v| parse_u64(v)),
            f.get(8).and_then(|v| parse_u64(v)),
        ) else {
            continue;
        };
        let (r, t) = total.unwrap_or((0, 0));
        total = Some((r.saturating_add(rx), t.saturating_add(tx)));
    }
    total
}

fn parse_load(s: &str) -> Option<[f32; 3]> {
    let v: Vec<f32> = s
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse::<f32>().ok())
        .filter(|f| f.is_finite() && *f >= 0.0)
        .take(3)
        .collect();
    (v.len() == 3).then(|| [v[0], v[1], v[2]])
}

fn parse_stat(line: &str) -> Option<CpuTimes> {
    let mut it = line.split_whitespace();
    if it.next()? != "cpu" {
        return None;
    }
    let fields: Vec<u64> = it.filter_map(parse_u64).collect();
    if fields.len() < 4 {
        return None;
    }
    let total = fields.iter().fold(0u64, |a, b| a.saturating_add(*b));
    // idle + iowait
    let idle = fields[3].saturating_add(fields.get(4).copied().unwrap_or(0));
    Some(CpuTimes { total, idle })
}

/// Pseudo file systems that are not worth a row.
const SKIP_FS: &[&str] = &[
    "tmpfs", "devtmpfs", "udev", "none", "shm", "overlay", "squashfs", "devfs", "map", "efivarfs", "cgroup", "proc",
    "sysfs",
];
const SKIP_MOUNT_PREFIX: &[&str] = &[
    "/snap/",
    "/run/",
    "/dev/",
    "/sys/",
    "/proc/",
    "/boot/efi",
    "/System/Volumes/",
];

fn parse_df_line(line: &str) -> Option<Disk> {
    // Filesystem 1024-blocks Used Available Capacity Mounted on
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 6 {
        return None;
    }
    let fs = parts[0];
    let total = parse_u64(parts[1])?;
    let used = parse_u64(parts[2])?;
    let mount = parts[5..].join(" ");
    if total == 0 || used > total {
        return None;
    }
    let fs_lower = fs.to_ascii_lowercase();
    if SKIP_FS
        .iter()
        .any(|s| fs_lower == *s || fs_lower.starts_with(&format!("{s}:")))
    {
        return None;
    }
    if SKIP_MOUNT_PREFIX.iter().any(|p| mount.starts_with(p)) {
        return None;
    }
    Some(Disk {
        mount: clean(&mount),
        total_kb: total,
        used_kb: used,
    })
}

/// `1d 03:04` style uptime.
pub fn format_uptime(secs: u64) -> String {
    let days = secs / 86_400;
    let h = (secs % 86_400) / 3600;
    let m = (secs % 3600) / 60;
    if days > 0 {
        format!("{days}d {h:02}:{m:02}")
    } else {
        format!("{h:02}:{m:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINUX: &str = "@@HOST\nweb-01\n@@UNAME\nLinux 6.8.0-45-generic x86_64\n@@OSREL\nNAME=\"Ubuntu\"\nPRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\n@@UPTIME\n93784.21 180000.00\n@@LOAD\n0.52 0.40 0.31 2/612 48213\n@@UPCMD\n 10:00:01 up 1 day,  2:03,  3 users,  load average: 0.52, 0.40, 0.31\n@@STAT\ncpu  10000 200 3000 80000 500 0 100 0 0 0\n@@NCPU\n8\n@@MEM\nMemTotal:       16384000 kB\nMemFree:         2000000 kB\nMemAvailable:    9000000 kB\nBuffers:          100000 kB\nSwapTotal:       4000000 kB\nSwapFree:        3500000 kB\n@@DF\nFilesystem     1024-blocks      Used Available Capacity Mounted on\n/dev/nvme0n1p2   491000000 210000000 256000000      46% /\ntmpfs              8000000         0   8000000       0% /dev/shm\n/dev/sda1       1900000000 900000000 900000000      50% /mnt/data disk\n/dev/loop3           64000     64000         0     100% /snap/core/1\n@@WHO\n3\n@@PS\n250\n@@NET\nInter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n    lo: 5000000   40000    0    0    0     0          0         0  5000000   40000    0    0    0     0       0          0\n  eth0: 1000000000 800000    0    0    0     0          0         0 200000000  500000    0    0    0     0       0          0\n wlan0: 1000       10    0    0    0     0          0         0     2000      20    0    0    0     0       0          0\ndocker0: 77777777   999    0    0    0     0          0         0 88888888     999    0    0    0     0       0          0\n@@END\n";

    #[test]
    fn parses_a_linux_sample() {
        let s = parse(LINUX, None);
        assert_eq!(s.hostname, "web-01");
        assert_eq!(s.kernel, "Linux 6.8.0-45-generic x86_64");
        assert_eq!(s.os, "Ubuntu 24.04.1 LTS");
        assert_eq!(s.uptime_secs, Some(93784));
        assert_eq!(s.load, Some([0.52, 0.40, 0.31]));
        assert_eq!(s.ncpu, Some(8));
        assert_eq!(
            s.cpu,
            Some(CpuTimes {
                total: 93800,
                idle: 80500
            })
        );
        assert_eq!(s.cpu_pct, None, "needs two samples");
        assert_eq!(s.mem_total_kb, Some(16_384_000));
        assert_eq!(s.mem_avail_kb, Some(9_000_000));
        assert!((s.mem_used_pct().unwrap() - 45.07).abs() < 0.1);
        assert!((s.swap_used_pct().unwrap() - 12.5).abs() < 0.1);
        assert_eq!(s.disks.len(), 2, "tmpfs and /snap are skipped: {:?}", s.disks);
        assert_eq!(s.disks[0].mount, "/");
        assert_eq!(s.disks[1].mount, "/mnt/data disk");
        assert!((s.disks[0].used_pct() - 42.77).abs() < 0.1);
        assert_eq!(s.users, Some(3));
        assert_eq!(s.procs, Some(249));
        assert_eq!(format_uptime(93784), "1d 02:03");
        assert_eq!(format_uptime(3661), "01:01");
    }

    #[test]
    fn cpu_percentage_from_two_samples() {
        let first = parse(LINUX, None);
        let second_txt = LINUX.replace(
            "cpu  10000 200 3000 80000 500 0 100 0 0 0",
            "cpu  10600 200 3200 80700 500 0 100 0 0 0",
        );
        let second = parse(&second_txt, Some(&first));
        // delta total = 1500, delta idle = 700 → 53.3 % busy
        assert!((second.cpu_pct.unwrap() - 53.33).abs() < 0.1, "{:?}", second.cpu_pct);
        // counters going backwards (reboot) never panic or report nonsense
        let third = parse(LINUX, Some(&second));
        assert_eq!(third.cpu_pct, None);
    }

    #[test]
    fn network_rates_from_two_samples() {
        let first = parse(LINUX, None);
        // lo and docker0 are left out of the total
        assert_eq!(
            first.net.map(|n| (n.rx_bytes, n.tx_bytes)),
            Some((1_000_001_000, 200_002_000))
        );
        assert_eq!(first.net_rx_rate, None, "needs two samples");
        let second_txt = LINUX
            .replace("93784.21 180000.00", "93794.21 180000.00")
            .replace("eth0: 1000000000", "eth0: 1010000000")
            .replace("0 200000000  500000", "0 202000000  500000");
        let second = parse(&second_txt, Some(&first));
        // 10 MB down and 2 MB up over 10 s
        assert!(
            (second.net_rx_rate.unwrap() - 1_000_000.0).abs() < 1.0,
            "{:?}",
            second.net_rx_rate
        );
        assert!(
            (second.net_tx_rate.unwrap() - 200_000.0).abs() < 1.0,
            "{:?}",
            second.net_tx_rate
        );
        // uptime / counters going backwards give no rate
        let third = parse(LINUX, Some(&second));
        assert_eq!(third.net_rx_rate, None);
        assert_eq!(third.net_tx_rate, None);
    }

    #[test]
    fn degrades_without_proc_and_sanitises() {
        let mac = "@@HOST\nmac\x1b[31m-mini\n@@UNAME\nDarwin 23.6.0 arm64\n@@OSREL\n@@UPTIME\n@@LOAD\n@@UPCMD\n10:00  up 5 days, 12:01, 2 users, load averages: 1.20 1.05 0.98\n@@STAT\n@@NCPU\n10\n@@MEM\n@@DF\nFilesystem 1024-blocks Used Available Capacity Mounted on\n/dev/disk3s1s1 971000000 10000000 500000000 2% /\ndevfs 200 200 0 100% /dev\n@@WHO\n2\n@@PS\n@@END\n";
        let s = parse(mac, None);
        assert_eq!(s.hostname, "mac[31m-mini", "escape stripped");
        assert_eq!(s.load, Some([1.20, 1.05, 0.98]));
        assert_eq!(s.uptime_secs, None);
        assert_eq!(s.cpu, None);
        assert_eq!(s.mem_total_kb, None);
        assert_eq!(s.mem_used_pct(), None);
        assert_eq!(s.disks.len(), 1);
        assert_eq!(s.procs, None);
        // garbage never panics
        let g = parse(
            "@@STAT\ncpu x y z\n@@MEM\nMemTotal: lots\n@@DF\nfoo\n@@NCPU\n-3\n",
            None,
        );
        assert_eq!(g.cpu, None);
        assert_eq!(g.ncpu, None);
        assert!(g.disks.is_empty());
        let _ = parse("", None);
    }
}
