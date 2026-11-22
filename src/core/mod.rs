pub mod agent_guard;
pub mod applock;
pub mod audit;
pub mod config;
pub mod forward;
pub mod history;
pub mod hostkeys;
pub mod i18n;
pub mod import;
pub mod keygen;
pub mod local_pty;
pub mod monitor;
pub mod platform;
pub mod sftp;
pub mod ssh;
pub mod tail;
pub mod term;
pub mod theme;
pub mod vault;
pub mod x11;
#[cfg(ruxt_embedded_xserver)]
pub mod xbundle;
pub mod xserver;

/// Identifier of a UI tab / session instance.
pub type TabId = u64;
