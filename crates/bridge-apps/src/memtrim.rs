// SPDX-License-Identifier: GPL-3.0-or-later
//! Hands freed heap back to the OS. glibc keeps memory it has freed inside the arena, and the GL
//! driver frees a lot of it right after start-up: with a cold NVIDIA shader cache the first
//! frames compile shaders in-process (libnvidia-gpucomp) and leave ~40 MB of freed-but-resident
//! heap behind (calls: 111 MB RSS cold vs 68 MB warm; see docs/performance.md). `malloc_trim(0)`
//! returns those pages. It is called a couple of times after start-up and when the window stops
//! being the one in use, never per frame. A no-op where the C library has no `malloc_trim`.
use std::sync::atomic::{AtomicU32, Ordering};

/// Frames after which a trim runs (the GL context and first data have settled by then).
const AT_FRAMES: [u32; 2] = [3, 90];

static FRAMES: AtomicU32 = AtomicU32::new(0);
static WAS_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn trim() {
    unsafe extern "C" {
        fn malloc_trim(pad: usize) -> i32;
    }
    // SAFETY: plain glibc call, safe from any thread at any time.
    unsafe {
        malloc_trim(0);
    }
    bridge_core::codepages::drop_idle();
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn trim() {}

/// Call once per frame with whether the window is focused and visible.
pub fn on_frame(active: bool) {
    let frame = FRAMES.fetch_add(1, Ordering::Relaxed).saturating_add(1);
    let went_idle = WAS_ACTIVE.swap(active, Ordering::Relaxed) && !active;
    if AT_FRAMES.contains(&frame) || went_idle {
        #[cfg(feature = "memstats")]
        crate::memstats::mark(&format!("frame {frame} before trim"));
        trim();
        #[cfg(feature = "memstats")]
        crate::memstats::mark(&format!("frame {frame} after trim"));
    }
}

/// Widest glyph-atlas texture we allow. egui sizes its font atlas as wide as the GPU allows (16384
/// px on NVIDIA, f32 per pixel, plus an RGBA copy for the upload), so a handful of glyphs costs
/// ~4 MB of CPU memory and the same again in the driver. These UIs draw a few hundred distinct
/// glyphs; 2048 px holds them with room to grow (the atlas gets taller, and egui rebuilds it if it
/// ever fills).
pub const MAX_ATLAS_SIDE: usize = 2048;

/// `eframe::App::raw_input_hook` body shared by every app.
pub fn cap_atlas(raw_input: &mut eframe::egui::RawInput) {
    if let Some(side) = raw_input.max_texture_side.as_mut() {
        *side = (*side).min(MAX_ATLAS_SIDE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlas_width_is_capped_but_never_raised() {
        let mut raw = eframe::egui::RawInput {
            max_texture_side: Some(16384),
            ..Default::default()
        };
        cap_atlas(&mut raw);
        assert_eq!(raw.max_texture_side, Some(MAX_ATLAS_SIDE));
        raw.max_texture_side = Some(1024);
        cap_atlas(&mut raw);
        assert_eq!(raw.max_texture_side, Some(1024));
        raw.max_texture_side = None;
        cap_atlas(&mut raw);
        assert_eq!(raw.max_texture_side, None);
    }

    #[test]
    fn trim_is_callable() {
        trim();
    }
}
