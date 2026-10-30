//! Central area: welcome screen, live scan and review.

use eframe::egui;
use egui::{Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use crate::edit::CropRect;

use super::{App, Mode};

impl App {
    pub(super) fn central(&mut self, ui: &mut egui::Ui) {
        match self.mode {
            Mode::Idle => self.welcome(ui),
            Mode::Scanning => self.live_view(ui),
            Mode::Calibrating => {
                ui.vertical_centered(|ui| {
                    ui.add_space(30.0);
                    ui.heading("Calibrage des couleurs");
                    ui.label("Posez la souris à plat sur une feuille blanche vierge, sans impression au verso, et ne la bougez pas.");
                    ui.spinner();
                    if let Some(t) = &self.camera_tex {
                        ui.add(egui::Image::new(t).max_width(640.0));
                    }
                });
            }
            Mode::Review => self.review_view(ui),
        }
    }

    pub(super) fn welcome(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.heading(egui::RichText::new("Mulot-scan").size(30.0));
            ui.add_space(10.0);
            if self.scanner.is_some() || self.replay.is_some() {
                ui.label("Posez la souris sur le document, puis cliquez sur « Scanner »");
                if self.cfg.use_mouse_button {
                    ui.label("ou appuyez sur le bouton Scan de la souris.");
                }
                ui.label("Déplacez-la lentement jusqu'à couvrir toute la zone, puis arrêtez le scan.");
            } else {
                ui.label("Branchez la souris scanner (IRIScan Mouse ou Dacuda Scanner Mouse, USB).");
                ui.spinner();
            }
            if self.stitcher.lock().flat.is_none() && self.scanner.is_some() {
                ui.add_space(20.0);
                ui.colored_label(
                    Color32::from_rgb(220, 150, 50),
                    "Conseil : calibrez les couleurs une fois (Réglages → Calibrer) pour des fonds bien blancs.",
                );
            }
        });
    }

    pub(super) fn live_view(&mut self, ui: &mut egui::Ui) {
        let avail = ui.available_rect_before_wrap();
        let painter = ui.painter_at(avail);
        painter.rect_filled(avail, 0.0, Color32::from_gray(40));
        let (Some(p), Some(tex)) = (&self.preview, &self.preview_tex) else {
            ui.centered_and_justified(|ui| ui.label("En attente des images…"));
            return;
        };
        let img_size = Vec2::new(p.image.width() as f32, p.image.height() as f32);
        let s = (avail.width() / img_size.x).min(avail.height() / img_size.y).min(3.0);
        let size = img_size * s;
        let rect = Rect::from_center_size(avail.center(), size);
        painter.image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        // Current frame outline.
        let (fw, fh) = (p.frame_size.0 as f64 / 2.0, p.frame_size.1 as f64 / 2.0);
        let to_screen = |x: f64, y: f64| {
            let px = (x - p.bounds.x0 as f64) * p.scale as f64;
            let py = (y - p.bounds.y0 as f64) * p.scale as f64;
            let (ux, uy) =
                crate::stitch::orient_point(p.quarter_turns, px, py, p.unrotated.0 as f64, p.unrotated.1 as f64);
            Pos2::new(rect.min.x + ux as f32 * s, rect.min.y + uy as f32 * s)
        };
        let pose = self.last_step.pose;
        let corners = [(-fw, -fh), (fw, -fh), (fw, fh), (-fw, fh)].map(|(u, v)| {
            let (x, y) = pose.apply(u, v);
            to_screen(x, y)
        });
        let color = if self.last_step.lifted || self.last_step.searching {
            Color32::from_rgb(120, 160, 255)
        } else if self.last_step.too_fast {
            Color32::RED
        } else if self.last_step.lost {
            Color32::from_rgb(255, 160, 0)
        } else {
            Color32::from_rgb(80, 200, 120)
        };
        for i in 0..4 {
            painter.line_segment([corners[i], corners[(i + 1) % 4]], Stroke::new(2.0, color));
        }
        // Small live camera view.
        if let Some(cam) = &self.camera_tex {
            let ts = cam.size_vec2();
            let cs = ts * (240.0 / ts.x.max(ts.y));
            let r = Rect::from_min_size(avail.right_bottom() - cs - Vec2::splat(10.0), cs);
            painter.image(cam.id(), r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::WHITE), StrokeKind::Outside);
        }
        ui.allocate_rect(avail, Sense::hover());
    }

    pub(super) fn review_view(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.refresh_edited(&ctx);
        let Some(tex) = self.edited_tex.clone() else { return };
        let avail = ui.available_rect_before_wrap();
        let (response, painter) =
            ui.allocate_painter(avail.size(), if self.crop_mode { Sense::drag() } else { Sense::hover() });
        painter.rect_filled(avail, 0.0, Color32::from_gray(50));
        let ts = tex.size_vec2();
        let s = (avail.width() / ts.x).min(avail.height() / ts.y).min(4.0);
        let rect = Rect::from_center_size(avail.center(), ts * s);
        painter.image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        if self.crop_mode {
            let to_norm = |p: Pos2| ((p.x - rect.min.x) / rect.width(), (p.y - rect.min.y) / rect.height());
            if response.drag_started() {
                self.crop_drag = response.interact_pointer_pos();
            }
            if let (Some(start), Some(cur)) = (self.crop_drag, response.interact_pointer_pos())
                && response.dragged()
            {
                let (x0, y0) = to_norm(start);
                let (x1, y1) = to_norm(cur);
                self.crop_tmp = CropRect { x0, y0, x1, y1 }.normalized();
            }
            if response.drag_stopped() {
                self.crop_drag = None;
            }
            let c = self.crop_tmp.normalized();
            let cr = Rect::from_min_max(
                Pos2::new(rect.min.x + c.x0 * rect.width(), rect.min.y + c.y0 * rect.height()),
                Pos2::new(rect.min.x + c.x1 * rect.width(), rect.min.y + c.y1 * rect.height()),
            );
            let shade = Color32::from_black_alpha(140);
            painter.rect_filled(Rect::from_min_max(rect.min, Pos2::new(rect.max.x, cr.min.y)), 0.0, shade);
            painter.rect_filled(Rect::from_min_max(Pos2::new(rect.min.x, cr.max.y), rect.max), 0.0, shade);
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(rect.min.x, cr.min.y), Pos2::new(cr.min.x, cr.max.y)),
                0.0,
                shade,
            );
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(cr.max.x, cr.min.y), Pos2::new(rect.max.x, cr.max.y)),
                0.0,
                shade,
            );
            painter.rect_stroke(cr, 0.0, Stroke::new(2.0, Color32::from_rgb(80, 170, 255)), StrokeKind::Middle);
            painter.text(
                avail.left_top() + Vec2::new(10.0, 10.0),
                egui::Align2::LEFT_TOP,
                "Tracez le rectangle à conserver, puis « Valider le recadrage »",
                egui::FontId::proportional(14.0),
                Color32::WHITE,
            );
        }
    }
}
