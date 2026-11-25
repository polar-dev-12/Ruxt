//! Checks GitHub Releases for a newer version of Ruxt.
//!
//! Uses `ureq` with `rustls` (no OpenSSL / native-tls) on a background thread so the UI never
//! blocks. Only reads the release metadata (`tag_name`, `html_url`) and never downloads or
//! executes binaries automatically.

use serde::Deserialize;
use std::sync::mpsc;
use std::time::Duration;

const RELEASES_API: &str = "https://api.github.com/repos/polarbear138/ruxt/releases/latest";
const ALLOWED_URL_PREFIX: &str = "https://github.com/polarbear138/ruxt/";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    /// Version string without a leading `v` (e.g. `"1.2.0"`).
    pub version: String,
    /// Verified GitHub release page URL.
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum UpdateState {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(ReleaseInfo),
    Failed(String),
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
}

/// Parse a `major.minor.patch` version (with optional leading `v` and trailing `-prerelease`).
pub fn parse_semver(s: &str) -> Option<(u64, u64, u64)> {
    let trimmed = s.trim().trim_start_matches(['v', 'V']);
    let core = trimmed.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Returns `true` when `candidate` is strictly newer than `current`.
pub fn is_newer(current: &str, candidate: &str) -> bool {
    match (parse_semver(current), parse_semver(candidate)) {
        (Some(cur), Some(cand)) => cand > cur,
        _ => false,
    }
}

fn validate_release_url(url: &str) -> Option<String> {
    let u = url.trim();
    if u.starts_with(ALLOWED_URL_PREFIX) && !u.chars().any(char::is_control) {
        Some(u.to_string())
    } else {
        None
    }
}

/// Synchronously query GitHub Releases for the latest version.
pub fn check_latest() -> UpdateState {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .build();
    let agent: ureq::Agent = config.into();
    let ua = format!("ruxt/{}", env!("CARGO_PKG_VERSION"));
    let resp = agent
        .get(RELEASES_API)
        .header("User-Agent", &ua)
        .header("Accept", "application/vnd.github+json")
        .call();
    let mut resp = match resp {
        Ok(r) => r,
        Err(e) => return UpdateState::Failed(e.to_string()),
    };
    let rel: GhRelease = match resp.body_mut().read_json() {
        Ok(r) => r,
        Err(e) => return UpdateState::Failed(e.to_string()),
    };
    let current = env!("CARGO_PKG_VERSION");
    if is_newer(current, &rel.tag_name) {
        let version = rel.tag_name.trim().trim_start_matches(['v', 'V']).to_string();
        let url = validate_release_url(&rel.html_url)
            .unwrap_or_else(|| format!("{}/releases/latest", env!("CARGO_PKG_REPOSITORY")));
        UpdateState::Available(ReleaseInfo { version, url })
    } else {
        UpdateState::UpToDate
    }
}

/// Run `check_latest` on a background thread and wake `ctx` when the result arrives.
pub fn spawn_check(ctx: egui::Context) -> mpsc::Receiver<UpdateState> {
    let (tx, rx) = mpsc::channel();
    let _ = std::thread::Builder::new().name("update-check".into()).spawn(move || {
        let state = check_latest();
        let _ = tx.send(state);
        ctx.request_repaint();
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_parsing_and_comparison() {
        assert_eq!(parse_semver("1.1.0"), Some((1, 1, 0)));
        assert_eq!(parse_semver("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver("V2.0.1-rc1"), Some((2, 0, 1)));
        assert_eq!(parse_semver("not-a-version"), None);

        assert!(is_newer("1.1.0", "v1.1.1"));
        assert!(is_newer("1.1.0", "v1.2.0"));
        assert!(is_newer("1.9.9", "v2.0.0"));
        assert!(!is_newer("1.1.0", "v1.1.0"));
        assert!(!is_newer("1.1.1", "v1.1.0"));
    }

    #[test]
    fn release_url_must_stay_on_repo() {
        assert!(validate_release_url("https://github.com/polarbear138/ruxt/releases/tag/v1.2.0").is_some());
        assert!(validate_release_url("https://evil.example/ruxt").is_none());
        assert!(validate_release_url("javascript:alert(1)").is_none());
    }
}
