//! Ruxt — an SSH/SFTP workstation with secure defaults, written in pure Rust.
//!
//! Crate-wide hardening:
//! * `forbid(unsafe_code)` — the whole binary is memory-safe Rust.
//! * Secrets are wrapped in `secrecy`/`zeroize` types and never logged.
#![forbid(unsafe_code)]
#![warn(rust_2018_idioms)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod core;

use std::sync::Arc;

fn main() -> eframe::Result {
    // Logging: never log secrets. Default level is `info`; override with RUXT_LOG.
    let filter = tracing_subscriber::EnvFilter::try_from_env("RUXT_LOG")
        .or_else(|_| tracing_subscriber::EnvFilter::try_from_env("RUSSHTERM_LOG"))
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,russh=warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let rt = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(4)
            .thread_name("ruxt-io")
            .build()
            .expect("tokio runtime"),
    );

    // Rendering options live in the config, which is needed before the window exists. The app
    // loads the config again on construction (cheap, and keeps one source of truth).
    let render = core::config::Paths::discover()
        .map(|p| {
            let c = core::config::AppConfig::load(&p);
            (c.renderer, c.hw_accel, c.vsync)
        })
        .unwrap_or_default();
    let (renderer, hw_accel, vsync) = match std::env::var("RUXT_RENDERER").as_deref() {
        Ok("wgpu") => (core::config::Renderer::Wgpu, render.1, render.2),
        Ok("opengl") | Ok("glow") => (core::config::Renderer::OpenGl, render.1, render.2),
        _ => render,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 500.0])
            .with_title("Ruxt"),
        // We handle Ctrl+C / Ctrl+V ourselves: in a terminal Ctrl+C must reach the remote shell.
        clipboard_shortcuts: false,
        renderer: match renderer {
            core::config::Renderer::OpenGl => eframe::Renderer::Glow,
            core::config::Renderer::Wgpu => eframe::Renderer::Wgpu,
        },
        glow_options: eframe::egui_glow::GlowConfiguration {
            vsync,
            hardware_acceleration: match hw_accel {
                core::config::HwAccel::Preferred => eframe::egui_glow::HardwareAcceleration::Preferred,
                core::config::HwAccel::Required => eframe::egui_glow::HardwareAcceleration::Required,
                core::config::HwAccel::Off => eframe::egui_glow::HardwareAcceleration::Off,
            },
            ..Default::default()
        },
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            surface: eframe::egui_wgpu::SurfaceConfig {
                present_mode: if vsync {
                    eframe::wgpu::PresentMode::AutoVsync
                } else {
                    eframe::wgpu::PresentMode::AutoNoVsync
                },
                ..eframe::egui_wgpu::SurfaceConfig::LOW_LATENCY
            },
            ..Default::default()
        },
        ..Default::default()
    };

    let rt_handle = rt.handle().clone();
    eframe::run_native(
        "Ruxt",
        options,
        Box::new(move |cc| Ok(Box::new(app::RuxtApp::new(cc, rt_handle)))),
    )
}
