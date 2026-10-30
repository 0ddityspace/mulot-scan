//! Hardware access for the IRIScan Mouse / Dacuda "Scanner Mouse" (USB 274f:c001).

pub mod frame;
pub mod protocol;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use crossbeam_channel::{Receiver, Sender, unbounded};
use image::RgbImage;
use serde::{Deserialize, Serialize};

pub use frame::{FRAME_HEIGHT, FRAME_WIDTH, FrameParser, MouseData};
pub use protocol::Mcu;

pub const VENDOR_ID: u16 = 0x274f;
pub const PRODUCT_ID: u16 = 0xc001;

/// Access to the vendor extension unit.
pub trait XuControl: Send + Sync {
    /// Size of a selector in bytes (UVC `GET_LEN`).
    fn len(&self, selector: u8) -> Result<u16>;
    /// UVC `GET_CUR`.
    fn get(&self, selector: u8, buf: &mut [u8]) -> Result<()>;
    /// UVC `SET_CUR`.
    fn set(&self, selector: u8, buf: &[u8]) -> Result<()>;
}

/// A source of raw MJPEG frames.
pub trait FrameSource: Send {
    /// Returns the next raw frame, `Ok(None)` on timeout.
    fn next_frame(&mut self) -> Result<Option<Vec<u8>>>;
}

#[cfg(target_os = "linux")]
type Backend = linux::LinuxBackend;
#[cfg(windows)]
type Backend = windows::WindowsBackend;

/// Imaging parameters applied when a scan starts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
pub struct ImagingSettings {
    /// LED exposure in milliseconds (`None` = leave the firmware default).
    pub exposure_ms: Option<f64>,
    /// Analog gain 1.0..=32.0.
    pub gain: Option<f64>,
    /// Raw white balance gains (blue, red, green).
    pub white_balance: Option<(u8, u8, u8)>,
}

/// One decoded frame.
#[derive(Debug, Clone)]
pub struct Frame {
    pub index: u64,
    pub image: Arc<RgbImage>,
    pub data: MouseData,
}

#[derive(Debug, Clone)]
pub enum DeviceEvent {
    Frame(Arc<Frame>),
    /// The scan button changed state.
    Button(bool),
    Error(String),
    Stopped,
}

/// An opened scanner mouse.
pub struct Scanner {
    backend: Backend,
    mcu: Mcu,
    pub info: protocol::FirmwareInfo,
}

impl Scanner {
    pub fn open() -> Result<Self> {
        let backend = Backend::open()?;
        let xu = backend.xu()?;
        let mcu = Mcu::new(xu);
        let info = mcu.firmware_info().context("reading firmware information")?;
        log::info!("opened {} (protocol {}, info {:02x?})", backend.description(), info.protocol, info.raw);
        if info.protocol != 2 {
            log::warn!("untested extension-unit protocol variant {}", info.protocol);
        }
        Ok(Self { backend, mcu, info })
    }

    pub fn description(&self) -> String {
        self.backend.description()
    }

    pub fn mcu(&self) -> &Mcu {
        &self.mcu
    }

    /// Puts the mouse in scan mode and starts streaming. The returned session
    /// delivers [`DeviceEvent`]s until it is stopped.
    pub fn start(self: &Arc<Self>, settings: ImagingSettings, record_to: Option<&Path>) -> Result<ScanSession> {
        let mcu = self.mcu.clone();
        // Port of DaStart(): disable mouse mode, configure, stream, enter scan mode.
        mcu.disable_mouse().context("disabling mouse mode")?;
        let setup = (|| -> Result<()> {
            mcu.write_verified(protocol::reg::SCAN_SETUP, 0x60)?;
            if let Some(ms) = settings.exposure_ms {
                mcu.set_exposure_ms(ms)?;
            }
            if let Some(g) = settings.gain {
                mcu.set_gain(g)?;
            }
            if let Some((b, r, g)) = settings.white_balance {
                mcu.set_white_balance(b, r, g)?;
            }
            Ok(())
        })();
        if let Err(e) = setup {
            let _ = mcu.enable_mouse();
            return Err(e);
        }
        let stream = match self.backend.open_stream() {
            Ok(s) => s,
            Err(e) => {
                let _ = mcu.enable_mouse();
                return Err(e);
            }
        };
        let recorder = match record_to {
            Some(p) => Some(std::fs::File::create(p).with_context(|| format!("creating {}", p.display()))?),
            None => None,
        };
        ScanSession::spawn(stream, Some(mcu), recorder)
    }

    /// Polls the scan button (call outside of a scan session, e.g. from a timer).
    pub fn button_pressed(&self) -> Result<bool> {
        self.mcu.button_pressed()
    }

    /// Makes sure the mouse behaves like a normal mouse again.
    pub fn restore_mouse_mode(&self) -> Result<()> {
        if self.mcu.current_mode()? != protocol::mode::ENABLE_MOUSE.1 {
            self.mcu.enable_mouse()?;
        }
        Ok(())
    }
}

/// Plays back a recording made with `--record` (concatenated raw MJPEG frames).
pub struct ReplaySource {
    frames: Vec<Vec<u8>>,
    pos: usize,
    interval: Duration,
    last: Option<Instant>,
}

impl ReplaySource {
    pub fn open(path: &Path, fps: f64) -> Result<Self> {
        let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let frames = frame::split_mjpeg_stream(&data).into_iter().map(|f| f.to_vec()).collect();
        Ok(Self { frames, pos: 0, interval: Duration::from_secs_f64(1.0 / fps.max(1.0)), last: None })
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }
}

impl FrameSource for ReplaySource {
    fn next_frame(&mut self) -> Result<Option<Vec<u8>>> {
        if self.pos >= self.frames.len() {
            bail!("end of recording");
        }
        if let Some(last) = self.last {
            let elapsed = last.elapsed();
            if elapsed < self.interval {
                std::thread::sleep(self.interval - elapsed);
            }
        }
        self.last = Some(Instant::now());
        self.pos += 1;
        Ok(Some(self.frames[self.pos - 1].clone()))
    }
}

/// A running acquisition.
pub struct ScanSession {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub events: Receiver<DeviceEvent>,
}

impl ScanSession {
    /// Starts a capture thread on `source`. When `mcu` is given, scan mode is
    /// entered once streaming runs and mouse mode is restored at the end.
    pub fn spawn(
        mut source: Box<dyn FrameSource>,
        mcu: Option<Mcu>,
        mut recorder: Option<std::fs::File>,
    ) -> Result<Self> {
        let (tx, rx) = unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let thread = std::thread::Builder::new().name("scanner-capture".into()).spawn(move || {
            let result = capture_loop(&mut *source, mcu.as_ref(), &stop2, &tx, recorder.as_mut());
            drop(source);
            if let Some(m) = &mcu
                && let Err(e) = m.enable_mouse()
            {
                log::warn!("could not restore mouse mode: {e:#}");
            }
            if let Err(e) = result {
                let _ = tx.send(DeviceEvent::Error(format!("{e:#}")));
            }
            let _ = tx.send(DeviceEvent::Stopped);
        })?;
        Ok(Self { stop, thread: Some(thread), events: rx })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for ScanSession {
    fn drop(&mut self) {
        self.stop();
    }
}

fn capture_loop(
    source: &mut dyn FrameSource,
    mcu: Option<&Mcu>,
    stop: &AtomicBool,
    tx: &Sender<DeviceEvent>,
    mut recorder: Option<&mut std::fs::File>,
) -> Result<()> {
    use std::io::Write;
    let mut parser = FrameParser::new();
    let mut index = 0u64;
    let mut scan_mode = mcu.is_none();
    let mut last_button: Option<bool> = None;
    let mut last_poll = Instant::now();
    let mut waited = 0;
    while !stop.load(Ordering::SeqCst) {
        let Some(raw) = source.next_frame()? else {
            waited += 1;
            if waited > 5 {
                bail!("the scanner stopped sending images");
            }
            continue;
        };
        waited = 0;
        if !scan_mode {
            // Streaming runs: switch the MCU to scan mode (illumination + sensor data).
            if let Some(m) = mcu {
                m.start_scan_mode()?;
            }
            scan_mode = true;
            continue;
        }
        if let Some(r) = recorder.as_deref_mut() {
            r.write_all(&raw)?;
        }
        let Some((parsed, data)) = parser.parse(&raw) else {
            continue;
        };
        let image = match image::load_from_memory_with_format(&parsed.jpeg, image::ImageFormat::Jpeg) {
            Ok(img) => img.to_rgb8(),
            Err(e) => {
                log::debug!("dropping undecodable frame: {e}");
                continue;
            }
        };
        if image.width() != FRAME_WIDTH || image.height() != FRAME_HEIGHT {
            continue;
        }
        let f = Frame { index, image: Arc::new(image), data };
        index += 1;
        if tx.send(DeviceEvent::Frame(Arc::new(f))).is_err() {
            break;
        }
        if let Some(m) = mcu
            && last_poll.elapsed() > Duration::from_millis(100)
        {
            last_poll = Instant::now();
            if let Ok(b) = m.button_pressed() {
                if last_button.is_some_and(|l| l != b) {
                    let _ = tx.send(DeviceEvent::Button(b));
                }
                last_button = Some(b);
            }
        }
    }
    Ok(())
}
