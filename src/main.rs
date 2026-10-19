// Platform-neutral bundle processing is shared; desktop integration is selected here.
#[cfg(target_os = "macos")]
mod audio;
mod autocrop;
#[cfg(target_os = "macos")]
mod autocrop_lab;
#[cfg(target_os = "macos")]
mod capture;
#[cfg(target_os = "macos")]
mod delivery;
#[cfg(target_os = "macos")]
mod export;
mod handoff;
#[cfg(target_os = "macos")]
mod helper;
#[cfg(target_os = "macos")]
mod hex;
#[cfg(target_os = "macos")]
mod hold;
#[cfg(target_os = "macos")]
mod icons;
#[cfg(target_os = "macos")]
mod inbox;
mod keyframes;
#[cfg(target_os = "macos")]
mod laser_overlay;
mod model;
#[cfg(target_os = "macos")]
mod onboarding;
#[cfg(target_os = "macos")]
mod phone;
mod render;
#[cfg(target_os = "macos")]
mod retention;
#[cfg(target_os = "macos")]
mod routing;
#[cfg(target_os = "macos")]
mod settings;
mod sketch;
#[cfg(target_os = "macos")]
mod sparkle;
#[cfg(target_os = "macos")]
mod transcribe;
#[cfg(target_os = "macos")]
mod ui;
#[cfg(target_os = "macos")]
mod web;
mod windows;
#[cfg(target_os = "macos")]
mod zoom;

#[cfg(target_os = "macos")]
include!("main_macos.rs");

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::{make_bundle_id, movie_frames_at, now_epoch_ms};

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    linux::run()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("Scrawl supports macOS and Linux");
