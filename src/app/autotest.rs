//! Scripted UI walk-through for automated screenshots (`MULOT_SCAN_AUTOTEST=<dir>`).

use std::path::PathBuf;

use eframe::egui;
use image::RgbaImage;

use crate::edit::{CropRect, EraseMode, Stroke};

use super::{App, Mode, Tool};

pub(super) struct AutoTest {
    pub(super) dir: PathBuf,
    pub(super) stage: u32,
    pub(super) ticks: u32,
}

#[derive(Clone, Copy)]
enum DemoStep {
    Hold,
    /// Eraser at a point of the displayed image (fractions); `true` ends the stroke.
    Paint(f32, f32, bool),
}

/// Erases the last paragraph of the demonstration document, line by line.
fn demo_steps() -> Vec<DemoStep> {
    // (line centre, start, end) as fractions of the image.
    const LINES: [(f32, f32, f32); 3] = [(0.795, 0.03, 0.81), (0.846, 0.85, 0.11), (0.896, 0.11, 0.40)];
    let mut steps = vec![DemoStep::Hold; 12];
    for (y, x0, x1) in LINES {
        let n = ((x1 - x0).abs() / 0.022).ceil() as usize;
        for i in 0..=n {
            steps.push(DemoStep::Paint(x0 + (x1 - x0) * i as f32 / n as f32, y, i == n));
        }
        steps.extend([DemoStep::Hold; 4]);
    }
    steps.extend([DemoStep::Hold; 36]);
    steps
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
                t.stage = 10;
                t.ticks = 0;
            }
            // Eraser demonstration, one screenshot per step (assembled into a video).
            10 => {
                let tick = t.ticks as usize;
                if tick == 1 {
                    self.set_tool(Tool::Erase);
                    self.editor.erase_mode = EraseMode::Fill;
                    self.editor.brush = 30.0;
                    self.demo_zoom(1.9, 0.82);
                    return;
                }
                let Some(&step) = demo_steps().get(tick - 2) else {
                    t.stage = 3;
                    t.ticks = 0;
                    self.demo_end();
                    return;
                };
                match step {
                    DemoStep::Hold => {}
                    DemoStep::Paint(x, y, lift) => self.demo_erase(x, y, lift),
                }
                std::fs::create_dir_all(dir.join("gomme")).ok();
                Self::screenshot(ctx, dir.join(format!("gomme/{:03}.png", tick - 2)));
            }
            3 if t.ticks == 8 => {
                self.edited_dirty = true;
                self.set_tool(Tool::Crop);
                self.adjust.angle = 3.0;
                self.set_crop(CropRect { x0: 0.02, y0: 0.1, x1: 0.6, y1: 0.55 });
            }
            3 if t.ticks > 25 => {
                Self::screenshot(ctx, dir.join("4-recadrage.png"));
                t.stage = 4;
                t.ticks = 0;
            }
            4 if t.ticks == 5 => {
                self.validate_crop();
                self.set_tool(Tool::Erase);
                let points = (0..12).map(|i| (0.2 + i as f32 * 0.03, 0.3)).collect();
                self.apply_stroke(&Stroke { points, radius: 0.02, mode: EraseMode::Fill });
                let points = (0..12).map(|i| (0.2 + i as f32 * 0.03, 0.4)).collect();
                self.apply_stroke(&Stroke { points, radius: 0.02, mode: EraseMode::White });
            }
            4 if t.ticks > 25 => {
                Self::screenshot(ctx, dir.join("5-gomme.png"));
                t.stage = 5;
                t.ticks = 0;
                self.show_settings = true;
            }
            5 if t.ticks > 20 => {
                Self::screenshot(ctx, dir.join("6-reglages.png"));
                t.stage = 6;
                t.ticks = 0;
            }
            6 if t.ticks > 20 => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            _ => {}
        }
    }
}
