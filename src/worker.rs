//! Background threads: device hot-plug/button monitoring and the scan pipeline.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui;
use image::{RgbImage, RgbaImage};
use parking_lot::Mutex;

use crate::device::{DeviceEvent, MouseData, Scanner};
use crate::stitch::{Bounds, FlatField, StepInfo, Stitcher};

// ------------------------------------------------------------------ device manager

pub enum DeviceMsg {
    Connected(Arc<Scanner>),
    Disconnected(String),
    /// The scan button of the mouse was pressed while idle.
    Button,
}

pub struct DeviceManager {
    pub rx: Receiver<DeviceMsg>,
    busy: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl DeviceManager {
    pub fn spawn(ctx: egui::Context) -> Self {
        let (tx, rx) = unbounded();
        let busy = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (busy2, stop2) = (busy.clone(), stop.clone());
        std::thread::Builder::new()
            .name("device-manager".into())
            .spawn(move || device_loop(tx, busy2, stop2, ctx))
            .expect("spawning device manager");
        Self { rx, busy, stop }
    }

    /// While busy (a scan is running) the manager does not touch the device.
    pub fn set_busy(&self, busy: bool) {
        self.busy.store(busy, Ordering::SeqCst);
    }
}

impl Drop for DeviceManager {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn device_loop(tx: Sender<DeviceMsg>, busy: Arc<AtomicBool>, stop: Arc<AtomicBool>, ctx: egui::Context) {
    let mut scanner: Option<Arc<Scanner>> = None;
    let mut last_button: Option<bool> = None;
    let mut last_event = Instant::now() - Duration::from_secs(10);
    let mut was_busy = false;
    while !stop.load(Ordering::SeqCst) {
        match &scanner {
            None => match Scanner::open() {
                Ok(s) => {
                    // A previous crash may have left the mouse in scan mode.
                    if let Err(e) = s.restore_mouse_mode() {
                        log::warn!("restoring mouse mode: {e:#}");
                    }
                    let s = Arc::new(s);
                    scanner = Some(s.clone());
                    last_button = None;
                    let _ = tx.send(DeviceMsg::Connected(s));
                    ctx.request_repaint();
                }
                Err(e) => {
                    log::debug!("scanner not available: {e:#}");
                    sleep_unless(&stop, Duration::from_secs(2));
                    continue;
                }
            },
            Some(s) => {
                let is_busy = busy.load(Ordering::SeqCst);
                if is_busy {
                    was_busy = true;
                } else {
                    if was_busy {
                        // Re-sync the button state after a scan session.
                        last_button = None;
                        was_busy = false;
                    }
                    match s.button_pressed() {
                        Ok(b) => {
                            if let Some(prev) = last_button
                                && prev != b
                                && last_event.elapsed() > Duration::from_millis(600)
                            {
                                last_event = Instant::now();
                                let _ = tx.send(DeviceMsg::Button);
                                ctx.request_repaint();
                            }
                            last_button = Some(b);
                        }
                        Err(e) => {
                            scanner = None;
                            let _ = tx.send(DeviceMsg::Disconnected(format!("{e:#}")));
                            ctx.request_repaint();
                        }
                    }
                }
            }
        }
        sleep_unless(&stop, Duration::from_millis(150));
    }
}

fn sleep_unless(stop: &AtomicBool, d: Duration) {
    let end = Instant::now() + d;
    while Instant::now() < end && !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ------------------------------------------------------------------ scan pipeline

pub struct Preview {
    pub image: RgbaImage,
    pub bounds: Bounds,
    pub scale: f32,
    pub frame_size: (u32, u32),
    /// Clockwise quarter turns applied to `image` (see `orient_point`).
    pub quarter_turns: u8,
    /// Size of the preview before rotation.
    pub unrotated: (u32, u32),
}

pub enum UiMsg {
    Preview(Box<Preview>),
    Step {
        info: StepInfo,
        fps: f32,
        frames: u64,
        mouse: MouseData,
    },
    /// Last camera frame (for the live camera view / diagnostics).
    Camera(Arc<RgbImage>),
    Button,
    Stopped(Option<String>),
    Calibrated(Result<FlatField, String>),
}

/// Consumes device events, feeds the stitcher and publishes previews.
pub fn spawn_scan_worker(
    events: Receiver<DeviceEvent>,
    stitcher: Arc<Mutex<Stitcher>>,
    tx: Sender<UiMsg>,
    ctx: egui::Context,
    max_preview: u32,
) {
    std::thread::Builder::new()
        .name("scan-worker".into())
        .spawn(move || {
            let mut last_preview = Instant::now() - Duration::from_secs(1);
            let mut last_camera = Instant::now() - Duration::from_secs(1);
            let mut fps_t0 = Instant::now();
            let mut fps_n = 0u32;
            let mut fps = 0.0f32;
            let mut error = None;
            let mut dirty = false;
            loop {
                let ev = match events.recv_timeout(Duration::from_millis(200)) {
                    Ok(ev) => ev,
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                    Err(_) => break,
                };
                match ev {
                    DeviceEvent::Frame(f) => {
                        // Skip frames if we fall behind, but never more than every other one.
                        if events.len() > 4 && f.index % 2 == 1 {
                            continue;
                        }
                        let (info, frames) = {
                            let mut st = stitcher.lock();
                            let info = st.push(&f);
                            (info, st.frame_count())
                        };
                        dirty = true;
                        fps_n += 1;
                        if fps_t0.elapsed() > Duration::from_millis(500) {
                            fps = fps_n as f32 / fps_t0.elapsed().as_secs_f32();
                            fps_t0 = Instant::now();
                            fps_n = 0;
                        }
                        let _ = tx.send(UiMsg::Step { info, fps, frames, mouse: f.data });
                        if last_camera.elapsed() > Duration::from_millis(100) {
                            last_camera = Instant::now();
                            // Show the camera the same way round as the mosaic.
                            let settings = stitcher.lock().settings;
                            let mut cam = (*f.image).clone();
                            if settings.mirror {
                                image::imageops::flip_vertical_in_place(&mut cam);
                            }
                            let cam = crate::stitch::orient_image(settings.output_quarter_turns, &cam);
                            let _ = tx.send(UiMsg::Camera(Arc::new(cam)));
                        }
                    }
                    DeviceEvent::Button(_) => {
                        let _ = tx.send(UiMsg::Button);
                    }
                    DeviceEvent::Error(e) => error = Some(e),
                    DeviceEvent::Stopped => break,
                }
                if dirty && last_preview.elapsed() > Duration::from_millis(150) {
                    last_preview = Instant::now();
                    dirty = false;
                    if let Some(p) = make_preview(&stitcher, max_preview) {
                        let _ = tx.send(UiMsg::Preview(Box::new(p)));
                    }
                }
                ctx.request_repaint();
            }
            if let Some(p) = make_preview(&stitcher, max_preview) {
                let _ = tx.send(UiMsg::Preview(Box::new(p)));
            }
            let _ = tx.send(UiMsg::Stopped(error));
            ctx.request_repaint();
        })
        .expect("spawning scan worker");
}

fn make_preview(stitcher: &Mutex<Stitcher>, max_dim: u32) -> Option<Preview> {
    let st = stitcher.lock();
    let b = st.canvas().bounds()?;
    let scale = (max_dim as f32 / b.width().max(b.height()) as f32).min(1.0);
    let image = st.canvas().render(Some(b), scale, [0, 0, 0, 0])?;
    let q = st.settings.output_quarter_turns;
    let unrotated = image.dimensions();
    let image = crate::stitch::orient_image(q, &image);
    Some(Preview { image, bounds: b, scale, frame_size: (640, 296), quarter_turns: q, unrotated })
}

/// Collects frames of a white sheet and computes a flat-field correction.
pub fn spawn_calibration_worker(
    events: Receiver<DeviceEvent>,
    tx: Sender<UiMsg>,
    ctx: egui::Context,
    done: Arc<AtomicBool>,
) {
    std::thread::Builder::new()
        .name("calibration".into())
        .spawn(move || {
            let mut frames: Vec<Arc<RgbImage>> = Vec::new();
            let mut skipped = 0;
            let mut error = None;
            while frames.len() < 40 {
                match events.recv_timeout(Duration::from_secs(3)) {
                    Ok(DeviceEvent::Frame(f)) => {
                        // Let exposure settle first.
                        if skipped < 10 {
                            skipped += 1;
                            continue;
                        }
                        frames.push(f.image.clone());
                        let _ = tx.send(UiMsg::Camera(f.image.clone()));
                        ctx.request_repaint();
                    }
                    Ok(DeviceEvent::Error(e)) => {
                        error = Some(e);
                        break;
                    }
                    Ok(DeviceEvent::Stopped) => break,
                    Ok(_) => {}
                    Err(_) => {
                        error = Some("aucune image reçue".to_string());
                        break;
                    }
                }
            }
            done.store(true, Ordering::SeqCst);
            let result = match error {
                Some(e) => Err(e),
                None => {
                    let refs: Vec<&RgbImage> = frames.iter().map(|f| f.as_ref()).collect();
                    FlatField::from_white_frames(&refs).ok_or_else(|| "pas assez d'images".to_string())
                }
            };
            let _ = tx.send(UiMsg::Calibrated(result));
            ctx.request_repaint();
        })
        .expect("spawning calibration worker");
}
