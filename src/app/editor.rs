//! Image editor of the review screen: crop and straighten frame, eraser,
//! zoom and pan, undo and redo.

use std::sync::Arc;

use eframe::egui;
use egui::{Color32, CursorIcon, Key, Modifiers, PointerButton, Pos2, Rect, Sense, StrokeKind, Vec2};

use crate::edit::{self, Adjustments, CropRect, EraseMode, Patch, Stroke};

use super::App;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Tool {
    /// Look around: drag to pan, wheel to zoom.
    #[default]
    Hand,
    Crop,
    Erase,
}

enum HistoryEntry {
    /// Settings before the change.
    Adjust(Adjustments),
    /// Pixels before an eraser stroke, in the full image and its preview.
    Erase { full: Patch, small: Patch },
}

#[derive(Clone, Copy)]
enum CropDrag {
    /// Drawing a new frame from this corner (normalised coordinates).
    New(Pos2),
    Move {
        start: Pos2,
        orig: CropRect,
    },
    /// Moving the edges `hx` (-1 left, 1 right) and `hy` (-1 top, 1 bottom).
    Resize {
        hx: i8,
        hy: i8,
    },
    /// Turning the image; angles in degrees and radians.
    Rotate {
        start_angle: f32,
        start_pointer: f32,
    },
}

#[derive(Clone, Copy, PartialEq)]
enum CropHit {
    Rotate,
    Edge(i8, i8),
    Inside,
    Outside,
}

pub(super) struct Editor {
    pub(super) tool: Tool,
    /// Frame being edited with the crop tool, normalised in the straightened image.
    crop: CropRect,
    /// Settings when the crop tool was opened, restored on cancel.
    crop_entry: Adjustments,
    crop_drag: Option<CropDrag>,
    pub(super) erase_mode: EraseMode,
    /// Eraser radius, in pixels of the full image.
    pub(super) brush: f32,
    /// Stroke being painted, and its points on screen.
    stroke: Option<(Stroke, Vec<Pos2>)>,
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
    /// Settings recorded in the history (the latest undo point).
    committed: Adjustments,
    /// 1 = whole image visible.
    zoom: f32,
    pan: Vec2,
    /// Screen pixels per full-image pixel, for the zoom readout.
    scale: f32,
    /// Where the image was drawn in the last frame.
    view_rect: Rect,
    /// Pointer of a scripted demonstration (`autotest.rs`), drawn like the real one.
    demo_pointer: Option<Pos2>,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            tool: Tool::Hand,
            crop: CropRect::FULL,
            crop_entry: Adjustments::default(),
            crop_drag: None,
            erase_mode: EraseMode::Fill,
            brush: 20.0,
            stroke: None,
            undo: Vec::new(),
            redo: Vec::new(),
            committed: Adjustments::default(),
            zoom: 1.0,
            pan: Vec2::ZERO,
            scale: 1.0,
            view_rect: Rect::NOTHING,
            demo_pointer: None,
        }
    }
}

impl Editor {
    /// New image: empty history and default view, eraser settings kept.
    pub(super) fn reset(&mut self, adjust: Adjustments) {
        *self = Self { committed: adjust, erase_mode: self.erase_mode, brush: self.brush, ..Self::default() };
    }

    fn fit(&mut self) {
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
    }
}

const ZOOM_RANGE: std::ops::RangeInclusive<f32> = 0.25..=24.0;
const BRUSH_RANGE: std::ops::RangeInclusive<f32> = 2.0..=300.0;
const ACCENT: Color32 = Color32::from_rgb(80, 170, 255);

impl App {
    // ------------------------------------------------------------------ tools and history

    pub(super) fn set_tool(&mut self, tool: Tool) {
        if self.editor.tool == tool {
            return;
        }
        if self.editor.tool == Tool::Crop {
            self.validate_crop();
        }
        if tool == Tool::Crop {
            self.commit_adjust();
            self.editor.crop_entry = self.adjust;
            self.editor.crop = self.adjust.crop;
            self.edited_dirty = true;
        }
        self.editor.tool = tool;
        self.editor.stroke = None;
    }

    pub(super) fn set_crop(&mut self, crop: CropRect) {
        self.editor.crop = crop.normalized();
    }

    pub(super) fn validate_crop(&mut self) {
        if self.editor.tool == Tool::Crop {
            self.adjust.crop = self.editor.crop.normalized();
            self.editor.tool = Tool::Hand;
            self.editor.crop_drag = None;
            self.editor.fit();
            self.edited_dirty = true;
            self.commit_adjust();
        }
    }

    pub(super) fn cancel_crop(&mut self) {
        if self.editor.tool == Tool::Crop {
            self.adjust = self.editor.crop_entry;
            self.editor.tool = Tool::Hand;
            self.editor.crop_drag = None;
            self.edited_dirty = true;
        }
    }

    /// Turns or mirrors the image as shown, keeping its straightening and crop
    /// (also the frame being edited in the crop tool).
    pub(super) fn turn(&mut self, quarter_turns: u8, mirror: bool) {
        let frame = Adjustments { crop: self.editor.crop, ..self.adjust }.turned(quarter_turns, mirror);
        self.adjust = self.adjust.turned(quarter_turns, mirror);
        self.editor.crop = frame.crop;
        self.edited_dirty = true;
    }

    pub(super) fn auto_straighten(&mut self) {
        if let Some(small) = &self.small {
            let base = Adjustments { angle: 0.0, crop: CropRect::FULL, ..self.adjust };
            self.adjust.angle = crate::edit::detect_skew(&base.geometry(small));
            self.edited_dirty = true;
        }
    }

    /// Records the settings in the history once they stop changing.
    fn commit_adjust(&mut self) {
        if self.adjust != self.editor.committed {
            self.editor.undo.push(HistoryEntry::Adjust(self.editor.committed));
            self.editor.committed = self.adjust;
            self.editor.redo.clear();
        }
    }

    pub(super) fn can_undo(&self) -> bool {
        !self.editor.undo.is_empty() || self.adjust != self.editor.committed || self.editor.tool == Tool::Crop
    }

    pub(super) fn undo(&mut self) {
        if self.editor.tool == Tool::Crop {
            self.cancel_crop();
            return;
        }
        self.commit_adjust();
        if let Some(e) = self.editor.undo.pop() {
            let e = self.swap_history(e);
            self.editor.redo.push(e);
        }
    }

    pub(super) fn redo(&mut self) {
        self.cancel_crop();
        if let Some(e) = self.editor.redo.pop() {
            let e = self.swap_history(e);
            self.editor.undo.push(e);
        }
    }

    /// Restores a history entry and returns the state it replaced.
    fn swap_history(&mut self, e: HistoryEntry) -> HistoryEntry {
        self.edited_dirty = true;
        match e {
            HistoryEntry::Adjust(a) => {
                let current = std::mem::replace(&mut self.adjust, a);
                self.editor.committed = a;
                HistoryEntry::Adjust(current)
            }
            HistoryEntry::Erase { mut full, mut small } => {
                if let Some(r) = &mut self.result {
                    full.swap(Arc::make_mut(r));
                }
                if let Some(s) = &mut self.small {
                    small.swap(s);
                }
                HistoryEntry::Erase { full, small }
            }
        }
    }

    /// Zooms on a horizontal band of the image, `ny` being its centre as a
    /// fraction of the image height (scripted demonstration).
    pub(super) fn demo_zoom(&mut self, zoom: f32, ny: f32) {
        let fit = self.editor.view_rect.size() / self.editor.zoom;
        self.editor.zoom = zoom;
        self.editor.pan = Vec2::new(0.0, -(ny - 0.5) * fit.y * zoom);
    }

    /// Paints with the eraser at `(nx, ny)`, in fractions of the displayed
    /// image; `lift` ends the stroke (scripted demonstration).
    pub(super) fn demo_erase(&mut self, nx: f32, ny: f32, lift: bool) {
        let rect = self.editor.view_rect;
        let Some(src) = self.result.as_ref().map(|r| r.dimensions()) else { return };
        let p = rect.lerp_inside(Vec2::new(nx, ny));
        self.editor.demo_pointer = Some(p);
        let (x, y) = self.adjust.view_to_source(src, nx, ny, true);
        let point = (x / src.0 as f32, y / src.1 as f32);
        match &mut self.editor.stroke {
            Some((stroke, screen)) => {
                stroke.points.push(point);
                screen.push(p);
            }
            None => {
                let stroke =
                    Stroke { points: vec![point], radius: self.editor.brush / src.0 as f32, mode: self.editor.erase_mode };
                self.editor.stroke = Some((stroke, vec![p]));
            }
        }
        if lift && let Some((stroke, _)) = self.editor.stroke.take() {
            self.apply_stroke(&stroke);
        }
    }

    /// Ends a scripted demonstration: back to the whole, unedited image.
    pub(super) fn demo_end(&mut self) {
        self.editor.demo_pointer = None;
        while self.can_undo() {
            self.undo();
        }
        self.editor.redo.clear();
        self.set_tool(Tool::Hand);
        self.editor.fit();
    }

    pub(super) fn apply_stroke(&mut self, stroke: &Stroke) {
        self.commit_adjust();
        let (Some(full), Some(small)) = (&mut self.result, &mut self.small) else { return };
        let full = Arc::make_mut(full);
        match (edit::erase(full, stroke), edit::erase(small, stroke)) {
            (Some(full), Some(small)) => {
                self.editor.undo.push(HistoryEntry::Erase { full, small });
                self.editor.redo.clear();
            }
            // Too small to show on one of the two images: keep them identical.
            (Some(mut p), None) => p.swap(full),
            (None, Some(mut p)) => p.swap(small),
            (None, None) => {}
        }
        self.edited_dirty = true;
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let key = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        let shift_cmd = Modifiers::COMMAND | Modifiers::SHIFT;
        if key(shift_cmd, Key::Z) || key(Modifiers::COMMAND, Key::Y) {
            self.redo();
        } else if key(Modifiers::COMMAND, Key::Z) {
            self.undo();
        }
        if key(Modifiers::NONE, Key::Escape) {
            if self.editor.tool == Tool::Crop {
                self.cancel_crop();
            } else {
                self.set_tool(Tool::Hand);
            }
        }
        if key(Modifiers::NONE, Key::Enter) {
            self.validate_crop();
        }
        for (k, tool) in [(Key::H, Tool::Hand), (Key::C, Tool::Crop), (Key::E, Tool::Erase)] {
            if key(Modifiers::NONE, k) {
                self.set_tool(tool);
            }
        }
        if key(Modifiers::NONE, Key::Plus) || key(Modifiers::NONE, Key::Equals) {
            self.editor.zoom = (self.editor.zoom * 1.25).clamp(*ZOOM_RANGE.start(), *ZOOM_RANGE.end());
        }
        if key(Modifiers::NONE, Key::Minus) {
            self.editor.zoom = (self.editor.zoom / 1.25).clamp(*ZOOM_RANGE.start(), *ZOOM_RANGE.end());
        }
    }

    // ------------------------------------------------------------------ toolbar and options

    /// Bar above the image: history, tools and zoom.
    fn editor_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.add_enabled(self.can_undo(), egui::Button::new("Annuler")).on_hover_text("Ctrl+Z").clicked() {
                self.undo();
            }
            if ui
                .add_enabled(!self.editor.redo.is_empty(), egui::Button::new("Rétablir"))
                .on_hover_text("Ctrl+Y ou Ctrl+Maj+Z")
                .clicked()
            {
                self.redo();
            }
            ui.separator();
            let tools = [
                (Tool::Hand, "Déplacer", "Faire glisser l'image et zoomer à la molette (H)"),
                (Tool::Crop, "Recadrer / redresser", "Cadre de recadrage et poignée de rotation (C)"),
                (Tool::Erase, "Gomme", "Effacer une tache, un trait, une marque… (E)"),
            ];
            for (tool, label, hint) in tools {
                if ui.selectable_label(self.editor.tool == tool, label).on_hover_text(hint).clicked() {
                    self.set_tool(tool);
                }
            }
            ui.separator();
            if ui.button("−").on_hover_text("Dézoomer (−)").clicked() {
                self.editor.zoom = (self.editor.zoom / 1.25).max(*ZOOM_RANGE.start());
            }
            ui.label(format!("{:.0} %", self.editor.scale * 100.0));
            if ui.button("+").on_hover_text("Zoomer (+)").clicked() {
                self.editor.zoom = (self.editor.zoom * 1.25).min(*ZOOM_RANGE.end());
            }
            if ui.button("Ajuster").on_hover_text("Voir toute l'image").clicked() {
                self.editor.fit();
            }
        });
    }

    /// Options of the current tool, at the top of the side panel.
    pub(super) fn tool_options(&mut self, ui: &mut egui::Ui) {
        match self.editor.tool {
            Tool::Hand => {}
            Tool::Crop => {
                tool_frame(ui, "Recadrer et redresser", |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Angle");
                        if ui
                            .add(egui::Slider::new(&mut self.adjust.angle, -45.0..=45.0).suffix("°").step_by(0.1))
                            .changed()
                        {
                            self.edited_dirty = true;
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Redresser automatiquement").on_hover_text("D'après les lignes de texte").clicked()
                        {
                            self.auto_straighten();
                        }
                        if ui.button("0°").clicked() {
                            self.adjust.angle = 0.0;
                            self.edited_dirty = true;
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Pivoter à gauche").clicked() {
                            self.turn(3, false);
                        }
                        if ui.button("Pivoter à droite").clicked() {
                            self.turn(1, false);
                        }
                        if ui.button("Miroir").clicked() {
                            self.turn(0, true);
                        }
                    });
                    if ui.button("Cadre sur toute l'image").clicked() {
                        self.editor.crop = CropRect::FULL;
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let ok = egui::Button::new(egui::RichText::new("Valider").strong())
                            .fill(Color32::from_rgb(30, 110, 60));
                        if ui.add(ok).on_hover_text("Entrée").clicked() {
                            self.validate_crop();
                        }
                        if ui.button("Annuler").on_hover_text("Échap").clicked() {
                            self.cancel_crop();
                        }
                    });
                });
            }
            Tool::Erase => {
                tool_frame(ui, "Gomme", |ui| {
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.editor.erase_mode, EraseMode::Fill, "Adaptative")
                            .on_hover_text("Remplit avec les couleurs et le grain du papier autour");
                        ui.radio_value(&mut self.editor.erase_mode, EraseMode::White, "Blanche")
                            .on_hover_text("Peint en blanc pur");
                    });
                    ui.horizontal(|ui| {
                        ui.label("Taille");
                        ui.add(egui::Slider::new(&mut self.editor.brush, BRUSH_RANGE).logarithmic(true).suffix(" px"));
                    });
                    ui.weak("Maj + molette sur l'image : changer la taille.");
                });
            }
        }
    }

    // ------------------------------------------------------------------ view

    pub(super) fn review_view(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.shortcuts(&ctx);
        if self.editor.tool != Tool::Crop && !ctx.input(|i| i.pointer.any_down()) {
            self.commit_adjust();
        }
        self.editor_toolbar(ui);
        self.refresh_edited(&ctx);
        let Some(tex) = self.edited_tex.clone() else { return };
        let avail = ui.available_rect_before_wrap();
        let (response, painter) = ui.allocate_painter(avail.size(), Sense::click_and_drag());
        painter.rect_filled(avail, 0.0, Color32::from_gray(50));

        // Placement: fitted in the view (with room for the crop handles), then zoomed and panned.
        let ts = tex.size_vec2();
        let inner = avail.shrink(if self.editor.tool == Tool::Crop { 44.0 } else { 12.0 });
        let fit = ts * (inner.width() / ts.x).min(inner.height() / ts.y);
        let place = |e: &Editor| Rect::from_center_size(avail.center() + e.pan, fit * e.zoom);
        self.zoom_and_pan(&ctx, &response, place(&self.editor), avail.center(), fit);
        let rect = place(&self.editor);
        self.editor.view_rect = rect;
        painter.image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);

        // Screen pixels per full-image pixel.
        let crop = if self.editor.tool == Tool::Crop { CropRect::FULL } else { self.adjust.crop.normalized() };
        if let Some(r) = &self.result {
            let geo_w = if self.adjust.quarter_turns % 2 == 1 { r.height() } else { r.width() } as f32;
            self.editor.scale = rect.width() / (geo_w * (crop.x1 - crop.x0)).max(1.0);
        }

        let panning = response.dragged_by(PointerButton::Middle) || response.dragged_by(PointerButton::Secondary);
        let hint = match self.editor.tool {
            Tool::Hand => {
                if response.hovered() {
                    ctx.set_cursor_icon(if response.dragged() { CursorIcon::Grabbing } else { CursorIcon::Grab });
                }
                "Faites glisser pour déplacer l'image, molette pour zoomer, double-clic pour tout voir."
            }
            Tool::Crop => {
                self.crop_interaction(&ctx, &response, &painter, rect, panning);
                "Faites glisser les bords ou les coins du cadre, ou tracez-en un nouveau. \
                 Poignée ronde : redresser. Entrée : valider, Échap : annuler."
            }
            Tool::Erase => {
                self.erase_interaction(&ctx, &response, &painter, rect, panning);
                "Peignez sur ce qu'il faut effacer. Molette : zoom, Maj + molette : taille, \
                 clic droit : déplacer."
            }
        };
        if panning && response.hovered() {
            ctx.set_cursor_icon(CursorIcon::Grabbing);
        }
        let galley =
            painter.layout(hint.into(), egui::FontId::proportional(13.0), Color32::WHITE, avail.width() - 30.0);
        let pos = avail.left_bottom() + Vec2::new(10.0, -8.0 - galley.size().y);
        painter.rect_filled(Rect::from_min_size(pos, galley.size()).expand(5.0), 4.0, Color32::from_black_alpha(160));
        painter.galley(pos, galley, Color32::WHITE);
    }

    fn zoom_and_pan(&mut self, ctx: &egui::Context, response: &egui::Response, rect: Rect, center: Pos2, fit: Vec2) {
        let e = &mut self.editor;
        let drag_pans = response.dragged_by(PointerButton::Middle)
            || response.dragged_by(PointerButton::Secondary)
            || (response.dragged_by(PointerButton::Primary) && e.tool == Tool::Hand);
        if drag_pans {
            e.pan += response.drag_delta();
        }
        if response.double_clicked() && e.tool == Tool::Hand {
            e.fit();
        }
        let Some(cursor) = response.hover_pos() else { return };
        let (scroll, zoom_delta, shift) = ctx.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers.shift));
        if shift {
            // Shift turns the wheel into horizontal scrolling on some systems.
            let d = scroll.x + scroll.y;
            if e.tool == Tool::Erase && d != 0.0 {
                e.brush = (e.brush * (d * 0.004).exp()).clamp(*BRUSH_RANGE.start(), *BRUSH_RANGE.end());
            }
            return;
        }
        let factor = zoom_delta * (scroll.y * 0.002).exp();
        if (factor - 1.0).abs() > 1e-4 {
            let zoom = (e.zoom * factor).clamp(*ZOOM_RANGE.start(), *ZOOM_RANGE.end());
            // Keep the point under the cursor in place.
            let n = (cursor - rect.min) / rect.size();
            let size = fit * zoom;
            e.pan = cursor - n * size + size / 2.0 - center;
            e.zoom = zoom;
        }
    }

    fn erase_interaction(
        &mut self,
        ctx: &egui::Context,
        response: &egui::Response,
        painter: &egui::Painter,
        rect: Rect,
        panning: bool,
    ) {
        let Some(src) = self.result.as_ref().map(|r| r.dimensions()) else { return };
        let r_screen = self.editor.brush * self.editor.scale;
        let (pressed, down, pos) =
            ctx.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.interact_pos()));
        let to_source = |adj: &Adjustments, p: Pos2| {
            let n = (p - rect.min) / rect.size();
            let (x, y) = adj.view_to_source(src, n.x, n.y, true);
            (x / src.0 as f32, y / src.1 as f32)
        };
        if pressed
            && response.hovered()
            && !panning
            && let Some(p) = pos
        {
            let stroke = Stroke {
                points: vec![to_source(&self.adjust, p)],
                radius: self.editor.brush / src.0 as f32,
                mode: self.editor.erase_mode,
            };
            self.editor.stroke = Some((stroke, vec![p]));
        }
        if let Some((stroke, screen)) = &mut self.editor.stroke
            && self.editor.demo_pointer.is_none()
        {
            if down {
                if let Some(p) = pos
                    && screen.last().is_none_or(|l| l.distance(p) > (r_screen / 4.0).max(2.0))
                {
                    stroke.points.push(to_source(&self.adjust, p));
                    screen.push(p);
                }
            } else if let Some((stroke, _)) = self.editor.stroke.take() {
                self.apply_stroke(&stroke);
            }
        }
        // Stroke in progress, then the brush outline.
        if let Some((_, screen)) = &self.editor.stroke {
            let color = match self.editor.erase_mode {
                EraseMode::White => Color32::from_rgba_unmultiplied(255, 255, 255, 210),
                EraseMode::Fill => Color32::from_rgba_unmultiplied(80, 170, 255, 120),
            };
            for p in screen {
                painter.circle_filled(*p, r_screen, color);
            }
            painter.add(egui::Shape::line(screen.clone(), egui::Stroke::new(r_screen * 2.0, color)));
        }
        if let Some(p) = response.hover_pos().or(self.editor.demo_pointer)
            && !panning
        {
            painter.circle_stroke(p, r_screen + 1.0, egui::Stroke::new(1.0, Color32::BLACK));
            painter.circle_stroke(p, r_screen, egui::Stroke::new(1.5, Color32::WHITE));
            ctx.set_cursor_icon(CursorIcon::Crosshair);
        }
    }

    fn crop_interaction(
        &mut self,
        ctx: &egui::Context,
        response: &egui::Response,
        painter: &egui::Painter,
        rect: Rect,
        panning: bool,
    ) {
        let to_norm = |p: Pos2| {
            let n = (p - rect.min) / rect.size();
            Pos2::new(n.x.clamp(0.0, 1.0), n.y.clamp(0.0, 1.0))
        };
        let pointer_angle = |p: Pos2| (p.y - rect.center().y).atan2(p.x - rect.center().x);
        let frame = |c: CropRect| {
            Rect::from_min_max(rect.lerp_inside(Vec2::new(c.x0, c.y0)), rect.lerp_inside(Vec2::new(c.x1, c.y1)))
        };
        let cr = frame(self.editor.crop.normalized());
        let knob = Pos2::new(cr.center().x, cr.min.y - 26.0);
        let hit = |p: Pos2| {
            const TOL: f32 = 10.0;
            if p.distance(knob) < 13.0 {
                return CropHit::Rotate;
            }
            let side = |v: f32, lo: f32, hi: f32| {
                if (v - lo).abs() < TOL {
                    -1
                } else if (v - hi).abs() < TOL {
                    1
                } else {
                    0
                }
            };
            let (hx, hy) = (side(p.x, cr.min.x, cr.max.x), side(p.y, cr.min.y, cr.max.y));
            if (hx != 0 || hy != 0) && cr.expand(TOL).contains(p) {
                CropHit::Edge(hx, hy)
            } else if cr.contains(p) {
                CropHit::Inside
            } else {
                CropHit::Outside
            }
        };

        if response.drag_started_by(PointerButton::Primary)
            && !panning
            && let Some(start) = ctx.input(|i| i.pointer.press_origin())
        {
            self.editor.crop_drag = Some(match hit(start) {
                CropHit::Rotate => {
                    CropDrag::Rotate { start_angle: self.adjust.angle, start_pointer: pointer_angle(start) }
                }
                CropHit::Edge(hx, hy) => CropDrag::Resize { hx, hy },
                CropHit::Inside => CropDrag::Move { start: to_norm(start), orig: self.editor.crop.normalized() },
                CropHit::Outside => CropDrag::New(to_norm(start)),
            });
        }
        if response.drag_stopped() {
            self.editor.crop_drag = None;
        }
        if response.double_clicked() && cr.contains(response.interact_pointer_pos().unwrap_or_default()) {
            self.validate_crop();
            return;
        }
        if let (Some(drag), Some(p)) = (self.editor.crop_drag, response.interact_pointer_pos())
            && response.dragged_by(PointerButton::Primary)
        {
            let n = to_norm(p);
            let c = &mut self.editor.crop;
            const MIN: f32 = 0.02;
            match drag {
                CropDrag::New(a) => *c = CropRect { x0: a.x, y0: a.y, x1: n.x, y1: n.y }.normalized(),
                CropDrag::Move { start, orig } => {
                    let dx = (n.x - start.x).clamp(-orig.x0, 1.0 - orig.x1);
                    let dy = (n.y - start.y).clamp(-orig.y0, 1.0 - orig.y1);
                    *c = CropRect { x0: orig.x0 + dx, y0: orig.y0 + dy, x1: orig.x1 + dx, y1: orig.y1 + dy };
                }
                CropDrag::Resize { hx, hy } => {
                    *c = c.normalized();
                    match hx {
                        -1 => c.x0 = n.x.min(c.x1 - MIN),
                        1 => c.x1 = n.x.max(c.x0 + MIN),
                        _ => {}
                    }
                    match hy {
                        -1 => c.y0 = n.y.min(c.y1 - MIN),
                        1 => c.y1 = n.y.max(c.y0 + MIN),
                        _ => {}
                    }
                }
                CropDrag::Rotate { start_angle, start_pointer } => {
                    let mut d = (pointer_angle(p) - start_pointer).to_degrees();
                    if d > 180.0 {
                        d -= 360.0;
                    } else if d < -180.0 {
                        d += 360.0;
                    }
                    let angle = ((start_angle + d) * 10.0).round() / 10.0;
                    if angle != self.adjust.angle {
                        self.adjust.angle = angle.clamp(-45.0, 45.0);
                        self.edited_dirty = true;
                    }
                }
            }
        }

        // Cursor.
        let rotating = matches!(self.editor.crop_drag, Some(CropDrag::Rotate { .. }));
        if !panning && let Some(p) = response.hover_pos() {
            let h = match self.editor.crop_drag {
                Some(CropDrag::Resize { hx, hy }) => CropHit::Edge(hx, hy),
                Some(CropDrag::Rotate { .. }) => CropHit::Rotate,
                Some(CropDrag::Move { .. }) => CropHit::Inside,
                Some(CropDrag::New(_)) => CropHit::Outside,
                None => hit(p),
            };
            ctx.set_cursor_icon(match h {
                CropHit::Rotate if rotating => CursorIcon::Grabbing,
                CropHit::Rotate => CursorIcon::Grab,
                CropHit::Edge(hx, hy) if hx != 0 && hy != 0 => {
                    if hx == hy {
                        CursorIcon::ResizeNwSe
                    } else {
                        CursorIcon::ResizeNeSw
                    }
                }
                CropHit::Edge(_, 0) => CursorIcon::ResizeHorizontal,
                CropHit::Edge(..) => CursorIcon::ResizeVertical,
                CropHit::Inside => CursorIcon::Move,
                CropHit::Outside => CursorIcon::Crosshair,
            });
        }

        // Frame.
        let cr = frame(self.editor.crop.normalized());
        let shade = Color32::from_black_alpha(150);
        for r in [
            Rect::from_min_max(rect.min, Pos2::new(rect.max.x, cr.min.y)),
            Rect::from_min_max(Pos2::new(rect.min.x, cr.max.y), rect.max),
            Rect::from_min_max(Pos2::new(rect.min.x, cr.min.y), Pos2::new(cr.min.x, cr.max.y)),
            Rect::from_min_max(Pos2::new(cr.max.x, cr.min.y), Pos2::new(rect.max.x, cr.max.y)),
        ] {
            painter.rect_filled(r, 0.0, shade);
        }
        // Grid: thirds, and a fine grid while turning to line up the text.
        let lines = if rotating { 12 } else { 3 };
        let grid = egui::Stroke::new(1.0, Color32::from_white_alpha(if rotating { 110 } else { 70 }));
        for i in 1..lines {
            let t = i as f32 / lines as f32;
            let x = cr.min.x + cr.width() * t;
            let y = cr.min.y + cr.height() * t;
            painter.line_segment([Pos2::new(x, cr.min.y), Pos2::new(x, cr.max.y)], grid);
            painter.line_segment([Pos2::new(cr.min.x, y), Pos2::new(cr.max.x, y)], grid);
        }
        painter.rect_stroke(cr, 0.0, egui::Stroke::new(2.0, ACCENT), StrokeKind::Middle);
        // Handles: corners and middles of the edges.
        let outline = egui::Stroke::new(1.0, Color32::from_gray(30));
        for (fx, fy) in [(0.0, 0.0), (0.5, 0.0), (1.0, 0.0), (0.0, 0.5), (1.0, 0.5), (0.0, 1.0), (0.5, 1.0), (1.0, 1.0)]
        {
            let c = cr.lerp_inside(Vec2::new(fx, fy));
            let r = Rect::from_center_size(c, Vec2::splat(if fx == 0.5 || fy == 0.5 { 8.0 } else { 11.0 }));
            painter.rect_filled(r, 1.0, Color32::WHITE);
            painter.rect_stroke(r, 1.0, outline, StrokeKind::Outside);
        }
        // Rotation knob.
        let knob = Pos2::new(cr.center().x, cr.min.y - 26.0);
        painter.line_segment([Pos2::new(knob.x, cr.min.y), knob], egui::Stroke::new(2.0, ACCENT));
        painter.circle_filled(knob, 9.0, if rotating { ACCENT } else { Color32::WHITE });
        painter.circle_stroke(knob, 9.0, egui::Stroke::new(2.0, ACCENT));
        painter.text(
            knob + Vec2::new(16.0, 0.0),
            egui::Align2::LEFT_CENTER,
            format!("{:+.1}°", self.adjust.angle),
            egui::FontId::proportional(14.0),
            Color32::WHITE,
        );
    }
}

/// Highlighted group for the options of the active tool.
fn tool_frame(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style()).stroke(egui::Stroke::new(1.5, ACCENT)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(egui::RichText::new(title).strong().color(ACCENT));
        add(ui);
    });
    ui.add_space(6.0);
}
