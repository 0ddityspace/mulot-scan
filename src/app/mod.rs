//! Graphical user interface.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui;
use egui::{ColorImage, TextureHandle, TextureOptions, Vec2};
use image::{RgbImage, RgbaImage};
use parking_lot::Mutex;

use crate::config::{self, Config};
use crate::device::{MouseData, ReplaySource, ScanSession, Scanner};
use crate::edit::Adjustments;
use crate::export::{self, ExportJob, Format};
use crate::ocr::Tesseract;
use crate::stitch::{StepInfo, Stitcher};
use crate::worker::{self, DeviceManager, DeviceMsg, Preview, UiMsg};

mod autotest;
mod editor;
mod panels;
mod tray;
mod views;
mod windows;

use autotest::AutoTest;
use editor::{Editor, Tool};
use tray::{Tray, TrayCmd};

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

/// Command-line options of the graphical interface.
#[derive(Default)]
pub struct GuiOptions {
    /// Feed the interface from a recording instead of the mouse.
    pub replay: Option<PathBuf>,
    /// Also save the raw stream of every scan in this directory.
    pub record_dir: Option<PathBuf>,
    /// Start hidden in the notification area (login item).
    pub start_hidden: bool,
}

pub struct App {
    cfg: Config,
    mode: Mode,
    devices: DeviceManager,
    scanner: Option<Arc<Scanner>>,
    /// Short connection state shown in the toolbar.
    device_status: String,
    /// Device details, shown on hover.
    device_detail: String,
    session: Option<ScanSession>,
    stitcher: Arc<Mutex<Stitcher>>,
    ui_tx: Sender<UiMsg>,
    ui_rx: Receiver<UiMsg>,
    task_tx: Sender<TaskMsg>,
    task_rx: Receiver<TaskMsg>,
    replay: Option<PathBuf>,
    record_dir: Option<PathBuf>,

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
    editor: Editor,
    edited_tex: Option<TextureHandle>,
    edited_dirty: bool,

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
    /// Icon in the notification area (absent if the desktop has none).
    tray: Option<Tray>,
    /// The window is hidden in the notification area.
    hidden: bool,
    /// Quitting for real, not hiding in the notification area.
    quitting: bool,
    /// Launched at login (state read from the system).
    autostart: bool,
    /// Scripted UI walk-through used for automated screenshots (`MULOT_SCAN_AUTOTEST=<dir>`).
    autotest: Option<AutoTest>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: GuiOptions) -> Self {
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
            device_status: "Recherche de la souris…".into(),
            device_detail: "Branchez la souris scanner (IRIScan Mouse ou Dacuda Scanner Mouse, USB 274f:c001).".into(),
            session: None,
            stitcher: Arc::new(Mutex::new(stitcher)),
            ui_tx,
            ui_rx,
            task_tx,
            task_rx,
            replay: opts.replay,
            record_dir: opts.record_dir,
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
            editor: Editor::default(),
            edited_tex: None,
            edited_dirty: false,
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
            tray: Tray::new(&cc.egui_ctx).map_err(|e| log::warn!("notification area: {e:#}")).ok(),
            hidden: opts.start_hidden,
            quitting: false,
            autostart: crate::autostart::is_enabled(),
            autotest: std::env::var_os("MULOT_SCAN_AUTOTEST").map(|d| AutoTest {
                dir: PathBuf::from(d),
                stage: 0,
                ticks: 0,
            }),
        };
        if app.hidden && app.tray.is_none() {
            // Nowhere to hide: show the window after all.
            app.show_window(&cc.egui_ctx);
        }
        if app.replay.is_some() {
            app.device_status = "Mode relecture".into();
            app.device_detail = app.replay.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        }
        app
    }

    fn save_config(&mut self) {
        if let Err(e) = self.cfg.save() {
            self.error = Some(format!("Impossible d'enregistrer les réglages : {e:#}"));
        }
    }

    // ------------------------------------------------------------------ scanning

    fn show_window(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        self.hidden = false;
    }

    fn poll_tray(&mut self, ctx: &egui::Context) {
        let cmds: Vec<TrayCmd> = self.tray.as_ref().map(|t| t.rx.try_iter().collect()).unwrap_or_default();
        for cmd in cmds {
            match cmd {
                TrayCmd::Show => self.show_window(ctx),
                TrayCmd::Scan => {
                    self.show_window(ctx);
                    if matches!(self.mode, Mode::Idle | Mode::Review) {
                        self.start_scan(ctx);
                    }
                }
                TrayCmd::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        // Closing the window only hides it; the mouse button brings it back.
        if ctx.input(|i| i.viewport().close_requested())
            && !self.quitting
            && self.tray.is_some()
            && self.cfg.close_to_tray
            && cfg!(windows)
        {
            if self.mode == Mode::Scanning {
                self.finish_scan(None);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.hidden = true;
        }
    }

    fn start_scan(&mut self, ctx: &egui::Context) {
        if self.mode == Mode::Scanning || self.mode == Mode::Calibrating {
            return;
        }
        {
            let mut st = self.stitcher.lock();
            st.settings = self.cfg.stitch;
            st.reset();
        }
        // Raw stream saved for this scan (`--record`).
        let mut recording = None;
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
            recording = self.record_dir.as_deref().map(next_recording_path);
            match scanner.start(self.cfg.imaging, recording.as_deref()) {
                Ok(s) => s,
                Err(e) => {
                    self.devices.set_busy(false);
                    self.error = Some(start_error_message(&e));
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
        self.status = match recording {
            Some(p) => format!("Scan en cours, enregistré dans {}", p.display()),
            None => "Scan en cours : déplacez lentement la souris sur le document.".into(),
        };
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
        self.adjust = Adjustments::default();
        self.editor.reset(self.adjust);
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
                    self.device_status = "Souris connectée".into();
                    self.device_detail = s.description();
                    self.scanner = Some(s);
                }
                DeviceMsg::Disconnected(e) => {
                    log::info!("device lost: {e}");
                    self.scanner = None;
                    self.device_status = "Souris déconnectée".into();
                    self.device_detail = e;
                }
                DeviceMsg::Button => {
                    if self.cfg.use_mouse_button && matches!(self.mode, Mode::Idle | Mode::Review) {
                        self.show_window(ctx);
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
        let img = if self.editor.tool == Tool::Crop { self.adjust.geometry(small) } else { self.adjust.apply(small) };
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
}

impl eframe::App for App {
    /// Runs even while the window is hidden in the notification area.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        self.poll_tray(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

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

/// First free `scan-NNN.mjpg` in `dir`.
/// Explains the most common start failure — the camera already in use — in plain words.
fn start_error_message(e: &anyhow::Error) -> String {
    let busy = e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>().is_some_and(|io| io.kind() == std::io::ErrorKind::ResourceBusy)
            || c.to_string().to_lowercase().contains("busy")
    });
    if busy {
        "La caméra de la souris est déjà utilisée par un autre programme. Fermez les autres fenêtres \
         de mulot-scan (ou le logiciel d'origine), puis relancez le scan."
            .into()
    } else {
        format!("Impossible de démarrer le scan : {e:#}")
    }
}

fn next_recording_path(dir: &Path) -> PathBuf {
    (1..).map(|n| dir.join(format!("scan-{n:03}.mjpg"))).find(|p| !p.exists()).expect("unbounded range")
}
