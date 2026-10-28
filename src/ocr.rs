//! Text recognition through the Tesseract command-line program.
//!
//! Tesseract is not linked: the `tesseract` executable is looked up on the
//! PATH (or in the default Windows install folder, or at a user-configured
//! location) and driven as a subprocess.  This keeps the build free of
//! native dependencies on both Windows and Linux.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use image::RgbImage;

#[derive(Clone, Debug)]
pub struct Tesseract {
    pub exe: PathBuf,
}

impl Tesseract {
    /// Locates the Tesseract executable.
    pub fn locate(configured: Option<&Path>) -> Option<Self> {
        if let Some(p) = configured.filter(|p| p.is_file()) {
            return Some(Self { exe: p.to_path_buf() });
        }
        if let Ok(p) = which::which("tesseract") {
            return Some(Self { exe: p });
        }
        #[cfg(windows)]
        {
            let candidates = [
                std::env::var_os("ProgramFiles").map(|p| PathBuf::from(p).join("Tesseract-OCR\\tesseract.exe")),
                std::env::var_os("ProgramFiles(x86)").map(|p| PathBuf::from(p).join("Tesseract-OCR\\tesseract.exe")),
                std::env::var_os("LOCALAPPDATA")
                    .map(|p| PathBuf::from(p).join("Programs\\Tesseract-OCR\\tesseract.exe")),
            ];
            for c in candidates.into_iter().flatten() {
                if c.is_file() {
                    return Some(Self { exe: c });
                }
            }
        }
        None
    }

    fn command(&self) -> Command {
        let mut c = Command::new(&self.exe);
        c.stdin(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            c.creation_flags(CREATE_NO_WINDOW);
        }
        c
    }

    pub fn version(&self) -> Result<String> {
        let out = self.command().arg("--version").output()?;
        let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        Ok(text.lines().next().unwrap_or_default().trim().to_string())
    }

    /// Installed language packs (without `osd`).
    pub fn languages(&self) -> Result<Vec<String>> {
        let out = self.command().arg("--list-langs").output().context("running tesseract --list-langs")?;
        let text = String::from_utf8_lossy(&out.stdout);
        Ok(text
            .lines()
            .skip_while(|l| !l.contains("List of available languages"))
            .skip(1)
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && l != "osd" && l != "equ")
            .collect())
    }

    fn write_input(img: &RgbImage, dir: &Path) -> Result<PathBuf> {
        let p = dir.join("page.png");
        img.save(&p).context("writing temporary image")?;
        Ok(p)
    }

    fn base_args(c: &mut Command, input: &Path, output: &str, lang: &str, dpi: u32) {
        c.arg(input).arg(output).arg("-l").arg(lang).arg("--dpi").arg(dpi.to_string()).arg("--psm").arg("3");
    }

    /// Recognises the text of `img`.
    pub fn recognize(&self, img: &RgbImage, lang: &str, dpi: u32) -> Result<String> {
        let dir = tempfile::tempdir()?;
        let input = Self::write_input(img, dir.path())?;
        let mut c = self.command();
        Self::base_args(&mut c, &input, "stdout", lang, dpi);
        let out = c.output().context("running tesseract")?;
        if !out.status.success() {
            bail!("tesseract failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).replace('\u{c}', "").trim_end().to_string())
    }

    /// Writes a searchable PDF (image + invisible text layer) to `dest`.
    pub fn searchable_pdf(&self, img: &RgbImage, lang: &str, dpi: u32, dest: &Path) -> Result<()> {
        let dir = tempfile::tempdir()?;
        let input = Self::write_input(img, dir.path())?;
        let base = dir.path().join("out");
        let mut c = self.command();
        Self::base_args(&mut c, &input, base.to_str().unwrap_or("out"), lang, dpi);
        c.arg("pdf");
        let out = c.output().context("running tesseract")?;
        if !out.status.success() {
            bail!("tesseract failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        std::fs::copy(base.with_extension("pdf"), dest).with_context(|| format!("writing {}", dest.display()))?;
        Ok(())
    }
}

/// Human-readable names of the most common Tesseract language codes.
pub fn language_name(code: &str) -> &str {
    match code {
        "fra" => "Français",
        "eng" => "Anglais",
        "deu" => "Allemand",
        "spa" => "Espagnol",
        "ita" => "Italien",
        "nld" => "Néerlandais",
        "por" => "Portugais",
        "afr" => "Afrikaans",
        "ara" => "Arabe",
        "chi_sim" => "Chinois simplifié",
        "chi_tra" => "Chinois traditionnel",
        "jpn" => "Japonais",
        "kor" => "Coréen",
        "rus" => "Russe",
        "pol" => "Polonais",
        "ces" => "Tchèque",
        "dan" => "Danois",
        "swe" => "Suédois",
        "nor" => "Norvégien",
        "fin" => "Finnois",
        "tur" => "Turc",
        "ell" => "Grec",
        "heb" => "Hébreu",
        "hun" => "Hongrois",
        "ron" => "Roumain",
        "ukr" => "Ukrainien",
        _ => code,
    }
}
