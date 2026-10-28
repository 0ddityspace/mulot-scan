//! Graphical user interface.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui;
use egui::{Color32, ColorImage, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, Vec2};
use image::{RgbImage, RgbaImage};
use parking_lot::Mutex;

use crate::config::{self, Config};
use crate::device::{MouseData, ReplaySource, ScanSession, Scanner};
use crate::edit::{Adjustments, ColorMode, CropRect};
use crate::export::{self, ExportJob, Format};
use crate::ocr::{self, Tesseract};
use crate::stitch::{StepInfo, Stitcher};
use crate::worker::{self, DeviceManager, DeviceMsg, Preview, UiMsg};

const PREVIEW_MAX: u32 = 1600;
const EDIT_PREVIEW_MAX: u32 = 1800;

#[derive(PartialEq, Eq, Clone, Copy)]
enum Mode {
    Idle,
    Scanning,
    Calibrating,
    Review,
}

enum TaskMsg {
    Ocr(Result<String, String>),
    Exported(Result<PathBuf, String>),
}

pub struct App {
    cfg: Config,
    mode: Mode,
    devices: DeviceManager,
    scanner: Option<Arc<Scanner>>,
    device_status: String,
    session: Option<ScanSession>,
    stitcher: Arc<Mutex<Stitcher>>,
    ui_tx: Sender<UiMsg>,
    ui_rx: Receiver<UiMsg>,
    task_tx: Sender<TaskMsg>,
    task_rx: Receiver<TaskMsg>,
    replay: Option<PathBuf>,

    // live scan
    preview: Option<Box<Preview>>,
    preview_tex: Option<TextureHandle>,
    camera_tex: Option<TextureHandle>,
    last_step: StepInfo,
    fps: f32,
    frames: u64,
    mouse: MouseData,

    // review
    result: Option<Arc<RgbImage>>,
    small: Option<RgbImage>,
    adjust: Adjustments,
    edited_tex: Option<TextureHandle>,
    edited_dirty: bool,
    crop_mode: bool,
    crop_drag: Option<Pos2>,
    crop_tmp: CropRect,

    // OCR
    tesseract: Option<Tesseract>,
    languages: Vec<String>,
    ocr_text: String,
    ocr_running: bool,
    export_running: bool,

    status: String,
    error: Option<String>,
    show_settings: bool,
    show_about: bool,
    calibration_done: Arc<AtomicBool>,
    /// Scripted UI walk-through used for automated screenshots (`MULOT_SCAN_AUTOTEST=<dir>`).
    autotest: Option<AutoTest>,
}

struct AutoTest {
    dir: PathBuf,
    stage: u32,
    ticks: u32,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, replay: Option<PathBuf>) -> Self {
        let cfg = Config::load();
        let mut stitcher = Stitcher::new(cfg.stitch);
        match config::load_flat_field() {
            Ok(f) => stitcher.flat = f,
            Err(e) => log::warn!("{e:#}"),
        }
        let (ui_tx, ui_rx) = unbounded();
        let (task_tx, task_rx) = unbounded();
        let tesseract = Tesseract::locate(cfg.tesseract_path.as_deref());
        let languages = tesseract.as_ref().and_then(|t| t.languages().ok()).unwrap_or_default();
        let mut cfg = cfg;
        if !languages.is_empty() && !cfg.ocr_lang.split('+').all(|l| languages.iter().any(|x| x == l)) {
            // Configured language pack missing: fall back to an installed one.
            if let Some(l) = ["fra", "eng"]
                .iter()
                .find(|l| languages.iter().any(|x| x == *l))
                .map(|s| s.to_string())
                .or_else(|| languages.first().cloned())
            {
                cfg.ocr_lang = l;
            }
        }
        let mut app = Self {
            devices: DeviceManager::spawn(cc.egui_ctx.clone()),
            cfg,
            mode: Mode::Idle,
            scanner: None,
            device_status: "Recherche de la souris scanner…".into(),
            session: None,
            stitcher: Arc::new(Mutex::new(stitcher)),
            ui_tx,
            ui_rx,
            task_tx,
            task_rx,
            replay,
            preview: None,
            preview_tex: None,
            camera_tex: None,
            last_step: StepInfo::default(),
            fps: 0.0,
            frames: 0,
            mouse: MouseData::default(),
            result: None,
            small: None,
            adjust: Adjustments::default(),
            edited_tex: None,
            edited_dirty: false,
            crop_mode: false,
            crop_drag: None,
            crop_tmp: CropRect::FULL,
            tesseract,
            languages,
            ocr_text: String::new(),
            ocr_running: false,
            export_running: false,
            status: String::new(),
            error: None,
            show_settings: false,
            show_about: false,
            calibration_done: Arc::new(AtomicBool::new(false)),
            autotest: std::env::var_os("MULOT_SCAN_AUTOTEST").map(|d| AutoTest {
                dir: PathBuf::from(d),
                stage: 0,
                ticks: 0,
            }),
        };
        if app.replay.is_some() {
            app.device_status = "Mode relecture".into();
        }
        app
    }

    fn save_config(&mut self) {
        if let Err(e) = self.cfg.save() {
            self.error = Some(format!("Impossible d'enregistrer les réglages : {e:#}"));
        }
    }

    // ------------------------------------------------------------------ scanning

    fn start_scan(&mut self, ctx: &egui::Context) {
        if self.mode == Mode::Scanning || self.mode == Mode::Calibrating {
            return;
        }
        {
            let mut st = self.stitcher.lock();
            st.settings = self.cfg.stitch;
            st.reset();
        }
        let session = if let Some(path) = self.replay.clone() {
            match ReplaySource::open(&path, 30.0).and_then(|src| ScanSession::spawn(Box::new(src), None, None)) {
                Ok(s) => s,
                Err(e) => {
                    self.error = Some(format!("{e:#}"));
                    return;
                }
            }
        } else {
            let Some(scanner) = self.scanner.clone() else {
                self.error = Some("La souris scanner n'est pas connectée.".into());
                return;
            };
            self.devices.set_busy(true);
            match scanner.start(self.cfg.imaging, None) {
                Ok(s) => s,
                Err(e) => {
                    self.devices.set_busy(false);
                    self.error = Some(format!("Impossible de démarrer le scan : {e:#}"));
                    return;
                }
            }
        };
        worker::spawn_scan_worker(
            session.events.clone(),
            self.stitcher.clone(),
            self.ui_tx.clone(),
            ctx.clone(),
            PREVIEW_MAX,
        );
        self.session = Some(session);
        self.preview = None;
        self.frames = 0;
        self.mode = Mode::Scanning;
        self.status = "Scan en cours : déplacez lentement la souris sur le document.".into();
    }

    fn stop_scan(&mut self) {
        if let Some(mut s) = self.session.take() {
            s.stop();
        }
        self.devices.set_busy(false);
    }

    fn finish_scan(&mut self, error: Option<String>) {
        self.stop_scan();
        if let Some(e) = error
            && !e.contains("end of recording")
        {
            self.error = Some(e);
        }
        let img = self.stitcher.lock().finish(self.cfg.auto_levels);
        match img {
            Some(img) => {
                self.set_result(img);
                self.status = "Scan terminé.".into();
            }
            None => {
                self.mode = Mode::Idle;
                self.status = "Aucune image capturée.".into();
            }
        }
    }

    fn set_result(&mut self, img: RgbImage) {
        let scale = (EDIT_PREVIEW_MAX as f32 / img.width().max(img.height()) as f32).min(1.0);
        let small = if scale < 1.0 {
            image::imageops::resize(
                &img,
                ((img.width() as f32 * scale) as u32).max(1),
                ((img.height() as f32 * scale) as u32).max(1),
                image::imageops::FilterType::Triangle,
            )
        } else {
            img.clone()
        };
        self.result = Some(Arc::new(img));
        self.small = Some(small);
        self.adjust = Adjustments { mirror: false, ..Adjustments::default() };
        self.crop_mode = false;
        self.edited_dirty = true;
        self.ocr_text.clear();
        self.mode = Mode::Review;
    }

    fn start_calibration(&mut self, ctx: &egui::Context) {
        let Some(scanner) = self.scanner.clone() else {
            self.error = Some("La souris scanner n'est pas connectée.".into());
            return;
        };
        self.devices.set_busy(true);
        match scanner.start(self.cfg.imaging, None) {
            Ok(s) => {
                self.calibration_done.store(false, std::sync::atomic::Ordering::SeqCst);
                worker::spawn_calibration_worker(
                    s.events.clone(),
                    self.ui_tx.clone(),
                    ctx.clone(),
                    self.calibration_done.clone(),
                );
                self.session = Some(s);
                self.mode = Mode::Calibrating;
                self.status = "Calibrage : laissez la souris immobile sur une feuille blanche…".into();
            }
            Err(e) => {
                self.devices.set_busy(false);
                self.error = Some(format!("Impossible de démarrer le calibrage : {e:#}"));
            }
        }
    }

    // ------------------------------------------------------------------ messages

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.devices.rx.try_recv() {
            match msg {
                DeviceMsg::Connected(s) => {
                    self.device_status = format!("Souris scanner connectée ({})", s.description());
                    self.scanner = Some(s);
                }
                DeviceMsg::Disconnected(e) => {
                    log::info!("device lost: {e}");
                    self.scanner = None;
                    self.device_status = "Souris scanner déconnectée".into();
                }
                DeviceMsg::Button => {
                    if self.cfg.use_mouse_button && matches!(self.mode, Mode::Idle | Mode::Review) {
                        self.start_scan(ctx);
                    }
                }
            }
        }
        while let Ok(msg) = self.ui_rx.try_recv() {
            match msg {
                UiMsg::Preview(p) => {
                    let ci = ColorImage::from_rgba_unmultiplied(
                        [p.image.width() as usize, p.image.height() as usize],
                        p.image.as_raw(),
                    );
                    match &mut self.preview_tex {
                        Some(t) => t.set(ci, TextureOptions::LINEAR),
                        None => self.preview_tex = Some(ctx.load_texture("mosaic", ci, TextureOptions::LINEAR)),
                    }
                    self.preview = Some(p);
                }
                UiMsg::Step { info, fps, frames, mouse } => {
                    self.last_step = info;
                    self.fps = fps;
                    self.frames = frames;
                    self.mouse = mouse;
                }
                UiMsg::Camera(img) => {
                    let rgba = RgbaImage::from_fn(img.width(), img.height(), |x, y| {
                        let p = img.get_pixel(x, y).0;
                        image::Rgba([p[0], p[1], p[2], 255])
                    });
                    let ci = ColorImage::from_rgba_unmultiplied(
                        [rgba.width() as usize, rgba.height() as usize],
                        rgba.as_raw(),
                    );
                    match &mut self.camera_tex {
                        Some(t) => t.set(ci, TextureOptions::LINEAR),
                        None => self.camera_tex = Some(ctx.load_texture("camera", ci, TextureOptions::LINEAR)),
                    }
                }
                UiMsg::Button => {
                    if self.cfg.use_mouse_button && self.mode == Mode::Scanning {
                        self.finish_scan(None);
                    }
                }
                UiMsg::Stopped(err) => {
                    if self.mode == Mode::Scanning {
                        self.finish_scan(err);
                    } else if let Some(e) = err {
                        self.error = Some(e);
                    }
                }
                UiMsg::Calibrated(res) => {
                    self.stop_scan();
                    self.mode = if self.result.is_some() { Mode::Review } else { Mode::Idle };
                    match res {
                        Ok(f) => {
                            if let Err(e) = config::save_flat_field(&f) {
                                self.error = Some(format!("{e:#}"));
                            }
                            self.stitcher.lock().flat = Some(f);
                            self.status = "Calibrage enregistré.".into();
                        }
                        Err(e) => self.error = Some(format!("Calibrage impossible : {e}")),
                    }
                }
            }
        }
        while let Ok(msg) = self.task_rx.try_recv() {
            match msg {
                TaskMsg::Ocr(r) => {
                    self.ocr_running = false;
                    match r {
                        Ok(t) => {
                            self.status = format!("Texte reconnu : {} caractères.", t.chars().count());
                            self.ocr_text = t;
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
                TaskMsg::Exported(r) => {
                    self.export_running = false;
                    match r {
                        Ok(p) => self.status = format!("Enregistré : {}", p.display()),
                        Err(e) => self.error = Some(e),
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------ review helpers

    fn final_image(&self) -> Option<RgbImage> {
        self.result.as_ref().map(|r| self.adjust.apply(r))
    }

    fn refresh_edited(&mut self, ctx: &egui::Context) {
        if !self.edited_dirty {
            return;
        }
        self.edited_dirty = false;
        let Some(small) = &self.small else { return };
        let img = if self.crop_mode { self.adjust.geometry(small) } else { self.adjust.apply(small) };
        let rgba = RgbaImage::from_fn(img.width(), img.height(), |x, y| {
            let p = img.get_pixel(x, y).0;
            image::Rgba([p[0], p[1], p[2], 255])
        });
        let ci = ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw());
        match &mut self.edited_tex {
            Some(t) => t.set(ci, TextureOptions::LINEAR),
            None => self.edited_tex = Some(ctx.load_texture("edited", ci, TextureOptions::LINEAR)),
        }
    }

    fn run_ocr(&mut self) {
        let Some(t) = self.tesseract.clone() else {
            self.error =
                Some("Tesseract est introuvable. Installez-le puis indiquez son chemin dans les réglages.".into());
            return;
        };
        let Some(img) = self.final_image() else { return };
        let lang = self.cfg.ocr_lang.clone();
        let dpi = self.cfg.dpi;
        let tx = self.task_tx.clone();
        self.ocr_running = true;
        self.status = "Reconnaissance du texte…".into();
        std::thread::spawn(move || {
            let r = t.recognize(&img, &lang, dpi).map_err(|e| format!("OCR : {e:#}"));
            let _ = tx.send(TaskMsg::Ocr(r));
        });
    }

    fn export(&mut self, format: Format) {
        let Some(img) = self.final_image() else { return };
        if format.needs_ocr() && format != Format::SearchablePdf && self.ocr_text.trim().is_empty() {
            self.error = Some("Lancez d'abord la reconnaissance de texte (bouton « Reconnaître le texte »).".into());
            return;
        }
        if format == Format::SearchablePdf && self.tesseract.is_none() {
            self.error = Some("Tesseract est nécessaire pour un PDF avec texte.".into());
            return;
        }
        let mut dialog = rfd::FileDialog::new()
            .set_file_name(format!("scan.{}", format.extension()))
            .add_filter(format.label(), &[format.extension()]);
        if let Some(d) = &self.cfg.last_dir {
            dialog = dialog.set_directory(d);
        }
        let Some(path) = dialog.save_file() else { return };
        self.cfg.last_dir = path.parent().map(Path::to_path_buf);
        self.save_config();
        let text = (!self.ocr_text.trim().is_empty()).then(|| self.ocr_text.clone());
        let tess = self.tesseract.clone();
        let lang = self.cfg.ocr_lang.clone();
        let (dpi, q, inc) = (self.cfg.dpi, self.cfg.jpeg_quality, self.cfg.docx_include_image);
        let tx = self.task_tx.clone();
        self.export_running = true;
        self.status = "Enregistrement…".into();
        std::thread::spawn(move || {
            let job = ExportJob {
                image: &img,
                text: text.as_deref(),
                dpi,
                jpeg_quality: q,
                ocr: tess.as_ref().map(|t| (t, lang.as_str())),
                docx_include_image: inc,
            };
            let r = export::export(&job, format, &path)
                .map(|_| path)
                .map_err(|e| format!("Enregistrement impossible : {e:#}"));
            let _ = tx.send(TaskMsg::Exported(r));
        });
    }

    fn open_image(&mut self) {
        let mut dialog = rfd::FileDialog::new().add_filter("Images", &["png", "jpg", "jpeg", "tif", "tiff", "bmp"]);
        if let Some(d) = &self.cfg.last_dir {
            dialog = dialog.set_directory(d);
        }
        let Some(path) = dialog.pick_file() else { return };
        match image::open(&path) {
            Ok(img) => {
                self.cfg.last_dir = path.parent().map(Path::to_path_buf);
                self.save_config();
                self.set_result(img.to_rgb8());
                self.status = format!("Image ouverte : {}", path.display());
            }
            Err(e) => self.error = Some(format!("Impossible d'ouvrir l'image : {e}")),
        }
    }

    // ------------------------------------------------------------------ panels

    fn toolbar(&mut self, ui: &mut egui::Ui) {
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

    fn status_bar(&mut self, ui: &mut egui::Ui) {
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

    fn side_panel(&mut self, ui: &mut egui::Ui) {
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

    fn language_picker(&mut self, ui: &mut egui::Ui) {
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

    fn central(&mut self, ui: &mut egui::Ui) {
        match self.mode {
            Mode::Idle => self.welcome(ui),
            Mode::Scanning => self.live_view(ui),
            Mode::Calibrating => {
                ui.vertical_centered(|ui| {
                    ui.add_space(30.0);
                    ui.heading("Calibrage des couleurs");
                    ui.label("Posez la souris à plat sur une feuille blanche propre et ne la bougez pas.");
                    ui.spinner();
                    if let Some(t) = &self.camera_tex {
                        ui.add(egui::Image::new(t).max_width(640.0));
                    }
                });
            }
            Mode::Review => self.review_view(ui),
        }
    }

    fn welcome(&mut self, ui: &mut egui::Ui) {
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

    fn live_view(&mut self, ui: &mut egui::Ui) {
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

    fn review_view(&mut self, ui: &mut egui::Ui) {
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

    fn settings_window(&mut self, ctx: &egui::Context) {
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

    fn about_window(&mut self, ctx: &egui::Context) {
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

fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, color);
}

impl App {
    fn screenshot(ctx: &egui::Context, path: PathBuf) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
            let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            if let Some(buf) = RgbaImage::from_raw(img.size[0] as u32, img.size[1] as u32, rgba) {
                let _ = buf.save(&path);
            }
        })));
    }

    fn autotest_step(&mut self, ctx: &egui::Context) {
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

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.mode == Mode::Review {
            egui::Panel::right("side").default_size(340.0).resizable(true).show(ui, |ui| self.side_panel(ui));
        }
        egui::CentralPanel::default_margins().show(ui, |ui| self.central(ui));

        if self.show_settings {
            self.settings_window(&ctx);
        }
        if self.show_about {
            self.about_window(&ctx);
        }
        if let Some(e) = self.error.clone() {
            let mut close = false;
            egui::Window::new("Erreur")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(&ctx, |ui| {
                    ui.label(e);
                    if ui.button("OK").clicked() {
                        close = true;
                    }
                });
            if close {
                self.error = None;
            }
        }
        if matches!(self.mode, Mode::Scanning | Mode::Calibrating) {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        self.autotest_step(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.stop_scan();
        let _ = self.cfg.save();
    }
}
