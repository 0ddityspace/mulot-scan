//! Settings and About windows.

use eframe::egui;
use egui::Color32;

use crate::config::{self};
use crate::ocr::Tesseract;

use super::{App, Mode};

impl App {
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        let mut changed = false;
        let mut calibrate = false;
        egui::Window::new("Réglages").open(&mut open).resizable(false).show(ctx, |ui| {
            ui.heading("Capture");
            let mut use_exp = self.cfg.imaging.exposure_ms.is_some();
            let mut exp = self.cfg.imaging.exposure_ms.unwrap_or(0.35);
            ui.horizontal(|ui| {
                changed |= ui.checkbox(&mut use_exp, "Exposition").changed();
                changed |= ui.add_enabled(use_exp, egui::Slider::new(&mut exp, 0.05..=2.0).suffix(" ms").logarithmic(true)).changed();
            });
            self.cfg.imaging.exposure_ms = use_exp.then_some(exp);
            changed |= ui.checkbox(&mut self.cfg.use_mouse_button, "Démarrer/arrêter avec le bouton Scan de la souris").changed();
            // Hiding the window is not possible on Wayland: Windows only.
            if cfg!(windows) {
                changed |= ui
                    .add_enabled(
                        self.tray.is_some(),
                        egui::Checkbox::new(&mut self.cfg.close_to_tray, "Fermer la fenêtre dans la zone de notification"),
                    )
                    .on_hover_text("Mulot-scan reste prêt : le bouton Scan de la souris rouvre la fenêtre et lance le scan. Pour quitter, utilisez le menu de l'icône.")
                    .on_disabled_hover_text("Aucune zone de notification n'est disponible sur ce bureau.")
                    .changed();
                let mut autostart = self.autostart;
                if ui
                    .checkbox(&mut autostart, "Lancer Mulot-scan à l'ouverture de session")
                    .on_hover_text("Démarre caché dans la zone de notification, prêt à scanner.")
                    .changed()
                {
                    match crate::autostart::set(autostart) {
                        Ok(()) => self.autostart = autostart,
                        Err(e) => self.error = Some(format!("Impossible de modifier le lancement au démarrage : {e:#}")),
                    }
                }
            }
            changed |= ui.checkbox(&mut self.cfg.stitch.mirror, "Corriger l'effet miroir de l'optique (à décocher si le texte apparaît inversé)").changed();
            ui.horizontal(|ui| {
                ui.label("Orientation du résultat");
                for (q, label) in [(0u8, "0°"), (3, "90° à gauche"), (1, "90° à droite"), (2, "180°")] {
                    changed |= ui.radio_value(&mut self.cfg.stitch.output_quarter_turns, q, label).changed();
                }
            });
            ui.horizontal(|ui| {
                ui.label("Sensibilité au soulèvement");
                changed |= ui.add(egui::Slider::new(&mut self.cfg.stitch.lift_sensitivity, 0..=150)).on_hover_text("Seuil de qualité de surface des capteurs laser en dessous duquel la souris est considérée comme levée (50 par défaut, 0 = luminosité seule).").changed();
            });
            changed |= ui
                .checkbox(&mut self.cfg.stitch.use_laser, "Utiliser les capteurs laser pour le suivi")
                .on_hover_text("Mesure le déplacement et la rotation même sur une zone blanche, et évite les erreurs de recalage sur les lignes régulières.")
                .changed();
            changed |= ui.checkbox(&mut self.cfg.stitch.auto_flat, "Correction automatique de l'éclairage (sans calibrage)").changed();
            changed |= ui.checkbox(&mut self.cfg.auto_levels, "Balance des blancs automatique").changed();
            ui.horizontal(|ui| {
                let has = self.stitcher.lock().flat.is_some();
                ui.label(if has { "Calibrage des couleurs : actif" } else { "Calibrage des couleurs : aucun" });
                if ui.add_enabled(self.scanner.is_some() && self.mode != Mode::Scanning, egui::Button::new("Calibrer…")).clicked() {
                    calibrate = true;
                }
                if ui.add_enabled(has, egui::Button::new("Supprimer")).clicked() {
                    let _ = config::delete_flat_field();
                    self.stitcher.lock().flat = None;
                }
            });
            ui.separator();
            ui.heading("Documents");
            ui.horizontal(|ui| {
                ui.label("Résolution des documents");
                changed |= ui.add(egui::DragValue::new(&mut self.cfg.dpi).range(72..=1200).suffix(" ppp")).changed();
            });
            ui.horizontal(|ui| {
                ui.label("Qualité JPEG");
                changed |= ui.add(egui::Slider::new(&mut self.cfg.jpeg_quality, 30..=100)).changed();
            });
            changed |= ui.checkbox(&mut self.cfg.docx_include_image, "Inclure l'image dans les documents Word").changed();
            ui.separator();
            ui.heading("Tesseract (OCR)");
            match &self.tesseract {
                Some(t) => {
                    ui.label(format!("{} — {}", t.exe.display(), t.version().unwrap_or_default()));
                }
                None => {
                    ui.colored_label(Color32::from_rgb(220, 150, 50), "Introuvable");
                }
            }
            if ui.button("Choisir l'exécutable tesseract…").clicked()
                && let Some(p) = rfd::FileDialog::new().pick_file() {
                    self.cfg.tesseract_path = Some(p);
                    self.tesseract = Tesseract::locate(self.cfg.tesseract_path.as_deref());
                    self.languages = self.tesseract.as_ref().and_then(|t| t.languages().ok()).unwrap_or_default();
                    changed = true;
                }
            if self.mode == Mode::Scanning {
                ui.separator();
                ui.heading("Diagnostic");
                ui.monospace(format!(
                    "compteur {:3}  mots {:?}\nqualité surface {:?}  extra {}  flags {:#04x}",
                    self.mouse.counter, self.mouse.words, self.mouse.surface_quality, self.mouse.extra, self.mouse.flags
                ));
            }
        });
        self.show_settings = open;
        if changed {
            self.stitcher.lock().settings = self.cfg.stitch;
            self.save_config();
        }
        if calibrate {
            self.start_calibration(ctx);
        }
    }

    pub(super) fn about_window(&mut self, ctx: &egui::Context) {
        egui::Window::new("À propos").open(&mut self.show_about).resizable(false).show(ctx, |ui| {
            ui.heading(format!("Mulot-scan {}", env!("CARGO_PKG_VERSION")));
            ui.label("Logiciel libre compatible avec la souris scanner IRIScan Mouse / Dacuda Scanner Mouse (USB 274f:c001).");
            ui.weak("Projet indépendant, sans lien avec I.R.I.S. ni Canon. IRIScan est une marque de ses propriétaires respectifs.");
            ui.label("Windows et Linux. OCR assuré par Tesseract.");
            if let Some(s) = &self.scanner {
                ui.separator();
                ui.monospace(format!("Périphérique : {}\nProtocole : {}", s.description(), s.info.protocol));
            }
        });
    }
}
