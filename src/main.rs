// On Windows, hide the spawn-a-console-window behaviour for release builds.
// `windows_subsystem = "windows"` says: "I'm a GUI app, not a console app".
// We only opt in for release; debug builds keep the console so `RUST_LOG`
// output is visible during development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! On-Screen Notes — entry point.
//!
//! What happens here:
//!   1. Initialise the env-var driven logger.
//!   2. Build a `NativeOptions` describing our root window (transparent,
//!      borderless, resizable, "always on top").
//!   3. Hand control to `eframe::run_native`, which spins up an event loop
//!      and calls `OnScreenNotesApp::new` once on startup.

mod app;
mod canvas;
mod config;
mod error;
mod flood_fill;
mod input;
mod interaction;
mod persistence;
mod platform;
mod recording;
mod screenshot;
mod tools;
mod ui;

use crate::app::OnScreenNotesApp;
use crate::error::Result;

fn main() -> Result<()> {
    // env_logger reads the `RUST_LOG` env var. The default keeps things
    // quiet; users can run `RUST_LOG=on_screen_notes=debug cargo run` for
    // verbose internal logs.
    // Default filter: our crate at `info`, everything else at `warn`. This
    // hides the avalanche of wgpu/Vulkan loader chatter while still showing
    // our own startup messages without needing RUST_LOG. User can override:
    //   RUST_LOG=on_screen_notes=debug,wgpu=info
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,on_screen_notes=info"),
    )
    .init();
    log::info!("on-screen-notes booting");

    // Use whichever backend wgpu was compiled with (Vulkan on Windows by
    // default, Metal on macOS, Vulkan/GL on Linux). Earlier we tried to
    // force DX12 on Windows to dodge OBS / NVIDIA Vulkan-layer stalls, but
    // wgpu's default feature set ships Vulkan only — restricting to DX12
    // gives "Failed to create surface for any enabled backend".
    // The real freeze was octotablet IRealTimeStylus subscribing during
    // App::new() before the window was shown; that is now fixed via
    // lazy init in `input::pen`.
    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        // wgpu present mode matters for input latency:
        //   * Fifo     = strict vsync; one frame queued — highest latency.
        //   * Mailbox  = vsynced but newest frame replaces queued; ~one frame
        //                less latency than Fifo, no tearing.
        //   * Immediate= no vsync; lowest latency but tearing possible.
        // Mailbox is the sweet-spot for a drawing app: feels closer to
        // Sticky Notes / OneNote without flicker on transparent windows.
        // wgpu falls back to Fifo silently if Mailbox isn't supported.
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            present_mode: eframe::wgpu::PresentMode::Mailbox,
            ..Default::default()
        },
        viewport: egui::ViewportBuilder::default()
            .with_title("on-screen-notes")
            // Borderless: removes the OS title bar and frame.
            .with_decorations(false)
            // Transparent: lets the desktop show through unpainted pixels.
            .with_transparent(true)
            // Resizable: edge-drag to resize even without a title bar.
            .with_resizable(true)
            // Initial size: large but not full-screen. User can resize.
            .with_inner_size([1200.0, 800.0])
            // NOTE: We deliberately do NOT set always-on-top on the main
            // window. On Windows, AlwaysOnTop + transparent + no-decorations
            // can produce a window that paints but never accepts focus —
            // looking "frozen" to the user. The tiny floating button is
            // still always-on-top so the user can summon the overlay.
            // Minimum size keeps the toolbar usable.
            .with_min_inner_size([320.0, 240.0]),
        // We rely on the OS compositor for transparency; tell eframe to
        // not paint a background colour itself.
        ..Default::default()
    };

    // `run_native` returns `Result<(), eframe::Error>`; map into anyhow.
    eframe::run_native(
        "on-screen-notes",
        native_options,
        Box::new(|cc| Ok(Box::new(OnScreenNotesApp::new(cc)))),
    )
    .map_err(|e| crate::error::anyhow!("eframe failed: {e}"))?;

    Ok(())
}
