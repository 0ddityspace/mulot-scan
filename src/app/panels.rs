//! Toolbar, status bar and the side panel of the review screen.

use eframe::egui;
use egui::{Color32, Sense, Vec2};

use crate::edit::{Adjustments, ColorMode, CropRect};
use crate::export::Format;
use crate::ocr::{self};

use super::{App, Mode};

impl App {
    pub(super) fn toolbar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            match self.mode {
                Mode::Scanning => {
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new("Arrêter le scan").strong())
                                .fill(Color32::from_rgb(170, 40, 40)),
                        )
                        .clicked()
                    {
                        self.finish_scan(None);
                    }
                }
                Mode::Calibrating => {
                    ui.add_enabled(false, egui::Button::new("Calibrage…"));
                }
                _ => {
                    let can = self.scanner.is_some() || self.replay.is_some();
                    let label = if self.mode == Mode::Review { "Nouveau scan" } else { "Scanner" };
                    if ui
                        .add_enabled(
                            can,
                            egui::Button::new(egui::RichText::new(label).strong()).fill(Color32::from_rgb(30, 110, 60)),
                        )
                        .clicked()
                    {
                        self.start_scan(&ctx);
                    }
                }
            }
            ui.separator();
            if ui
                .add_enabled(matches!(self.mode, Mode::Idle | Mode::Review), egui::Button::new("Ouvrir une image…"))
                .clicked()
            {
                self.open_image();
            }
            if ui.button("Réglages").clicked() {
                self.show_settings = true;
            }
            if ui.button("À propos").clicked() {
                self.show_about = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (color, text) = if self.scanner.is_some() || self.replay.is_some() {
                    (Color32::from_rgb(60, 170, 90), self.device_status.as_str())
                } else {
                    (Color32::from_rgb(200, 140, 40), self.device_status.as_str())
                };
                ui.label(text);
                status_dot(ui, color);
            });
        });
    }

    pub(super) fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.mode == Mode::Scanning {
                let (c, t) = if self.last_step.lifted {
                    (Color32::from_rgb(120, 160, 255), "Souris levée – reposez-la sur une zone déjà scannée")
                } else if self.last_step.searching {
                    (
                        Color32::from_rgb(120, 160, 255),
                        "Recherche de la position – placez la souris sur une zone déjà scannée",
                    )
                } else if self.last_step.too_fast {
                    (Color32::from_rgb(220, 60, 60), "Trop rapide !")
                } else if self.last_step.lost {
                    (Color32::from_rgb(230, 150, 40), "Position perdue – revenez sur une zone déjà scannée")
                } else if self.last_step.laser_only {
                    (Color32::from_rgb(60, 170, 90), "Suivi OK (capteurs laser)")
                } else {
                    (Color32::from_rgb(60, 170, 90), "Suivi OK")
                };
                status_dot(ui, c);
                ui.label(t);
                ui.separator();
                ui.label(format!("{:.0} img/s", self.fps));
                ui.label(format!("{} images", self.frames));
                ui.label(format!("vitesse {:.0} px/img", self.last_step.speed));
                ui.label(format!("recalage {:.0}", self.last_step.confidence));
                ui.separator();
            }
            ui.label(&self.status);
        });
    }

    pub(super) fn side_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Image");
            ui.horizontal(|ui| {
                if ui.button("Pivoter à gauche").clicked() {
                    self.adjust.quarter_turns = (self.adjust.quarter_turns + 3) % 4;
                    self.adjust.crop = CropRect::FULL;
                    self.edited_dirty = true;
                }
                if ui.button("Pivoter à droite").clicked() {
                    self.adjust.quarter_turns = (self.adjust.quarter_turns + 1) % 4;
                    self.adjust.crop = CropRect::FULL;
                    self.edited_dirty = true;
                }
                if ui.button("Miroir").clicked() {
                    self.adjust.mirror = !self.adjust.mirror;
                    self.adjust.crop = CropRect::FULL;
                    self.edited_dirty = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label("Redresser");
                if ui.add(egui::Slider::new(&mut self.adjust.angle, -15.0..=15.0).suffix("°").step_by(0.1)).changed() {
                    self.edited_dirty = true;
                }
            });
            if ui.button("Redresser automatiquement").clicked()
                && let Some(small) = &self.small
            {
                let base = Adjustments { angle: 0.0, crop: CropRect::FULL, ..self.adjust };
                let geo = base.geometry(small);
                self.adjust.angle = crate::edit::detect_skew(&geo);
                self.adjust.crop = CropRect::FULL;
                self.edited_dirty = true;
            }
            ui.horizontal(|ui| {
                let label = if self.crop_mode { "Valider le recadrage" } else { "Recadrer" };
                if ui.button(label).clicked() {
                    if self.crop_mode {
                        self.adjust.crop = self.crop_tmp.normalized();
                    } else {
                        self.crop_tmp = self.adjust.crop;
                    }
                    self.crop_mode = !self.crop_mode;
                    self.edited_dirty = true;
                }
                if ui.add_enabled(!self.adjust.crop.is_full() || self.crop_mode, egui::Button::new("Annuler")).clicked()
                {
                    self.adjust.crop = CropRect::FULL;
                    self.crop_tmp = CropRect::FULL;
                    self.crop_mode = false;
                    self.edited_dirty = true;
                }
            });
            ui.add_space(4.0);
            let mut changed = false;
            ui.horizontal(|ui| {
                ui.label("Luminosité");
                changed |= ui.add(egui::Slider::new(&mut self.adjust.brightness, -100.0..=100.0)).changed();
            });
            ui.horizontal(|ui| {
                ui.label("Contraste");
                changed |= ui.add(egui::Slider::new(&mut self.adjust.contrast, -100.0..=100.0)).changed();
            });
            ui.horizontal(|ui| {
                changed |= ui.radio_value(&mut self.adjust.mode, ColorMode::Color, "Couleur").changed();
                changed |= ui.radio_value(&mut self.adjust.mode, ColorMode::Gray, "Gris").changed();
                changed |= ui.radio_value(&mut self.adjust.mode, ColorMode::BlackWhite, "Noir et blanc").changed();
            });
            if self.adjust.mode == ColorMode::BlackWhite {
                ui.horizontal(|ui| {
                    ui.label("Seuil");
                    changed |= ui.add(egui::Slider::new(&mut self.adjust.bw_level, 0.0..=100.0)).changed();
                });
            }
            if ui.button("Réinitialiser les retouches").clicked() {
                self.adjust = Adjustments::default();
                self.crop_mode = false;
                changed = true;
            }
            if changed {
                self.edited_dirty = true;
            }
            if let Some(r) = &self.result {
                let (w, h) = r.dimensions();
                let dpi = self.cfg.dpi.max(1) as f32;
                ui.weak(format!(
                    "{w} × {h} px ({:.1} × {:.1} cm à {} ppp)",
                    w as f32 / dpi * 2.54,
                    h as f32 / dpi * 2.54,
                    self.cfg.dpi
                ));
            }

            ui.separator();
            ui.heading("Texte (OCR)");
            if self.tesseract.is_none() {
                ui.colored_label(
                    Color32::from_rgb(220, 150, 50),
                    "Tesseract n'est pas installé : l'OCR est indisponible.",
                );
                ui.hyperlink_to("Télécharger Tesseract", "https://tesseract-ocr.github.io/tessdoc/Installation.html");
            } else {
                self.language_picker(ui);
                ui.horizontal(|ui| {
                    if ui.add_enabled(!self.ocr_running, egui::Button::new("Reconnaître le texte")).clicked() {
                        self.run_ocr();
                    }
                    if self.ocr_running {
                        ui.spinner();
                    }
                    if ui.add_enabled(!self.ocr_text.is_empty(), egui::Button::new("Copier le texte")).clicked() {
                        ui.ctx().copy_text(self.ocr_text.clone());
                        self.status = "Texte copié dans le presse-papiers.".into();
                    }
                });
                egui::ScrollArea::vertical().id_salt("ocr_text").max_height(220.0).show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.ocr_text)
                            .desired_width(f32::INFINITY)
                            .desired_rows(8)
                            .hint_text(
                                "Le texte reconnu apparaîtra ici ; vous pouvez le corriger avant de l'enregistrer.",
                            ),
                    );
                });
            }

            ui.separator();
            ui.heading("Enregistrer");
            ui.add_enabled_ui(!self.export_running, |ui| {
                egui::Grid::new("export").num_columns(2).spacing([6.0, 6.0]).show(ui, |ui| {
                    for (i, f) in Format::ALL.iter().enumerate() {
                        let enabled = match f {
                            Format::SearchablePdf => self.tesseract.is_some(),
                            Format::Txt | Format::Docx => !self.ocr_text.trim().is_empty(),
                            _ => true,
                        };
                        if ui
                            .add_enabled(enabled, egui::Button::new(f.label()).min_size(Vec2::new(150.0, 0.0)))
                            .clicked()
                        {
                            self.export(*f);
                        }
                        if i % 2 == 1 {
                            ui.end_row();
                        }
                    }
                });
                if self.export_running {
                    ui.spinner();
                }
            });
        });
    }

    pub(super) fn language_picker(&mut self, ui: &mut egui::Ui) {
        let selected: Vec<String> =
            self.cfg.ocr_lang.split('+').filter(|s| !s.is_empty()).map(str::to_string).collect();
        let summary = selected.iter().map(|c| ocr::language_name(c).to_string()).collect::<Vec<_>>().join(" + ");
        let mut new_sel = selected.clone();
        egui::ComboBox::from_label("Langue(s)")
            .selected_text(if summary.is_empty() { "—".into() } else { summary })
            .show_ui(ui, |ui| {
                for code in &self.languages {
                    let mut on = new_sel.contains(code);
                    if ui.checkbox(&mut on, format!("{} ({code})", ocr::language_name(code))).changed() {
                        if on {
                            new_sel.push(code.clone());
                        } else {
                            new_sel.retain(|c| c != code);
                        }
                    }
                }
                if self.languages.is_empty() {
                    ui.label("Aucune langue installée pour Tesseract.");
                }
            });
        if new_sel != selected && !new_sel.is_empty() {
            self.cfg.ocr_lang = new_sel.join("+");
            self.save_config();
        }
        if !selected.iter().all(|c| self.languages.contains(c)) && !self.languages.is_empty() {
            ui.colored_label(
                Color32::from_rgb(220, 150, 50),
                "Langue non installée pour Tesseract : choisissez-en une autre.",
            );
        }
    }
}

pub(super) fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, color);
}
