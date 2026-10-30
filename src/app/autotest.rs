//! Scripted UI walk-through for automated screenshots (`MULOT_SCAN_AUTOTEST=<dir>`).

use std::path::PathBuf;

use eframe::egui;
use image::RgbaImage;

use crate::edit::{ColorMode, CropRect};

use super::{App, Mode};

pub(super) struct AutoTest {
    pub(super) dir: PathBuf,
    pub(super) stage: u32,
    pub(super) ticks: u32,
}

impl App {
    pub(super) fn screenshot(ctx: &egui::Context, path: PathBuf) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
            let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            if let Some(buf) = RgbaImage::from_raw(img.size[0] as u32, img.size[1] as u32, rgba) {
                let _ = buf.save(&path);
            }
        })));
    }

    pub(super) fn autotest_step(&mut self, ctx: &egui::Context) {
        let Some(t) = &mut self.autotest else { return };
        t.ticks += 1;
        ctx.request_repaint();
        let dir = t.dir.clone();
        match t.stage {
            0 if t.ticks > 20 => {
                Self::screenshot(ctx, dir.join("1-accueil.png"));
                t.stage = 1;
                t.ticks = 0;
                self.start_scan(ctx);
            }
            1 if self.mode == Mode::Scanning && self.frames > 120 && t.ticks > 5 => {
                Self::screenshot(ctx, dir.join("2-scan.png"));
                t.stage = 2;
                t.ticks = 0;
            }
            2 if self.mode == Mode::Review && t.ticks > 20 => {
                Self::screenshot(ctx, dir.join("3-revue.png"));
                t.stage = 3;
                t.ticks = 0;
            }
            3 if t.ticks == 8 => {
                self.adjust.mode = ColorMode::BlackWhite;
                self.crop_mode = true;
                self.crop_tmp = CropRect { x0: 0.02, y0: 0.1, x1: 0.6, y1: 0.55 };
                self.edited_dirty = true;
            }
            3 if t.ticks > 25 => {
                Self::screenshot(ctx, dir.join("4-recadrage.png"));
                t.stage = 4;
                t.ticks = 0;
                self.show_settings = true;
            }
            4 if t.ticks > 20 => {
                Self::screenshot(ctx, dir.join("5-reglages.png"));
                t.stage = 5;
                t.ticks = 0;
            }
            5 if t.ticks > 20 => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            _ => {}
        }
    }
}
