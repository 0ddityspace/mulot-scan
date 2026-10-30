//! Persistent user settings and calibration data.

use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::device::ImagingSettings;
use crate::stitch::{FlatField, StitchSettings};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub imaging: ImagingSettings,
    pub stitch: StitchSettings,
    /// Tesseract language string, e.g. "fra+eng".
    pub ocr_lang: String,
    pub tesseract_path: Option<PathBuf>,
    /// Resolution assumed for PDF/Word page sizes and OCR.
    pub dpi: u32,
    pub jpeg_quality: u8,
    pub auto_levels: bool,
    pub last_dir: Option<PathBuf>,
    pub docx_include_image: bool,
    /// Start/stop scanning with the button of the mouse.
    pub use_mouse_button: bool,
    /// Closing the window hides it in the notification area (Windows only: a
    /// Wayland window cannot be hidden).
    pub close_to_tray: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            imaging: ImagingSettings { exposure_ms: Some(0.35), gain: None, white_balance: None },
            stitch: StitchSettings::default(),
            ocr_lang: "fra".into(),
            tesseract_path: None,
            dpi: 300,
            jpeg_quality: 90,
            auto_levels: true,
            last_dir: None,
            docx_include_image: false,
            use_mouse_button: true,
            close_to_tray: cfg!(windows),
        }
    }
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("mulot-scan")
}

impl Config {
    fn path() -> PathBuf {
        config_dir().join("settings.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(config_dir())?;
        std::fs::write(Self::path(), serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

const FLAT_MAGIC: &[u8; 8] = b"MULOTFF1";
/// Magic of calibration files written before the project was renamed.
const OLD_FLAT_MAGIC: &[u8; 8] = b"IRISFF01";

fn flat_path() -> PathBuf {
    config_dir().join("flatfield.bin")
}

pub fn save_flat_field(f: &FlatField) -> Result<()> {
    std::fs::create_dir_all(config_dir())?;
    let mut out = std::io::BufWriter::new(std::fs::File::create(flat_path())?);
    out.write_all(FLAT_MAGIC)?;
    out.write_all(&f.width.to_le_bytes())?;
    out.write_all(&f.height.to_le_bytes())?;
    for g in &f.gains {
        for v in g {
            out.write_all(&v.to_le_bytes())?;
        }
    }
    out.flush()?;
    Ok(())
}

pub fn load_flat_field() -> Result<Option<FlatField>> {
    let p = flat_path();
    if !p.exists() {
        return Ok(None);
    }
    let mut data = Vec::new();
    std::fs::File::open(&p)?.read_to_end(&mut data).context("reading calibration")?;
    if data.len() < 16 || (&data[..8] != FLAT_MAGIC && &data[..8] != OLD_FLAT_MAGIC) {
        bail!("invalid calibration file {}", p.display());
    }
    let w = u32::from_le_bytes(data[8..12].try_into()?);
    let h = u32::from_le_bytes(data[12..16].try_into()?);
    let n = (w * h) as usize;
    if data.len() != 16 + n * 12 {
        bail!("truncated calibration file {}", p.display());
    }
    let gains = data[16..]
        .as_chunks::<12>()
        .0
        .iter()
        .map(|c| {
            let f = |i: usize| f32::from_le_bytes(c[i * 4..i * 4 + 4].try_into().unwrap());
            [f(0), f(1), f(2)]
        })
        .collect();
    // Calibrations made before the illumination model became robust may contain
    // the texture of the sheet (text, show-through): clean them on load.
    let flat = FlatField { width: w, height: h, gains }
        .sanitized()
        .with_context(|| format!("calibration {} unusable, please calibrate again", p.display()))?;
    Ok(Some(flat))
}

pub fn delete_flat_field() -> Result<()> {
    let p = flat_path();
    if p.exists() {
        std::fs::remove_file(p)?;
    }
    Ok(())
}
