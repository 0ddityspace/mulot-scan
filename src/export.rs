//! Saving scans: images, PDF (image or searchable), plain text and Word.

use std::io::{Cursor, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use image::codecs::jpeg::JpegEncoder;
use image::{ImageFormat, RgbImage};

use crate::ocr::Tesseract;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Tiff,
    Bmp,
    /// PDF containing only the image.
    Pdf,
    /// PDF with an invisible OCR text layer (needs Tesseract).
    SearchablePdf,
    /// Plain text (needs OCR text).
    Txt,
    /// Word document with the OCR text (and optionally the image).
    Docx,
}

impl Format {
    pub const ALL: [Format; 8] = [
        Format::Png,
        Format::Jpeg,
        Format::Tiff,
        Format::Bmp,
        Format::Pdf,
        Format::SearchablePdf,
        Format::Txt,
        Format::Docx,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Format::Png => "Image PNG",
            Format::Jpeg => "Image JPEG",
            Format::Tiff => "Image TIFF",
            Format::Bmp => "Image BMP",
            Format::Pdf => "PDF (image)",
            Format::SearchablePdf => "PDF avec texte (OCR)",
            Format::Txt => "Texte brut (OCR)",
            Format::Docx => "Document Word (OCR)",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg => "jpg",
            Format::Tiff => "tif",
            Format::Bmp => "bmp",
            Format::Pdf | Format::SearchablePdf => "pdf",
            Format::Txt => "txt",
            Format::Docx => "docx",
        }
    }

    pub fn needs_ocr(self) -> bool {
        matches!(self, Format::SearchablePdf | Format::Txt | Format::Docx)
    }
}

pub struct ExportJob<'a> {
    pub image: &'a RgbImage,
    pub text: Option<&'a str>,
    pub dpi: u32,
    pub jpeg_quality: u8,
    pub ocr: Option<(&'a Tesseract, &'a str)>,
    pub docx_include_image: bool,
}

pub fn export(job: &ExportJob, format: Format, dest: &Path) -> Result<()> {
    match format {
        Format::Png => job.image.save_with_format(dest, ImageFormat::Png)?,
        Format::Tiff => job.image.save_with_format(dest, ImageFormat::Tiff)?,
        Format::Bmp => job.image.save_with_format(dest, ImageFormat::Bmp)?,
        Format::Jpeg => std::fs::write(dest, encode_jpeg(job.image, job.jpeg_quality)?)?,
        Format::Pdf => std::fs::write(dest, image_pdf(job.image, job.dpi, job.jpeg_quality)?)?,
        Format::SearchablePdf => {
            let Some((t, lang)) = job.ocr else { bail!("Tesseract est nécessaire pour un PDF avec texte") };
            t.searchable_pdf(job.image, lang, job.dpi, dest)?;
        }
        Format::Txt => {
            let Some(text) = job.text else { bail!("lancez d'abord la reconnaissance de texte (OCR)") };
            std::fs::write(dest, text)?;
        }
        Format::Docx => {
            let Some(text) = job.text else { bail!("lancez d'abord la reconnaissance de texte (OCR)") };
            write_docx(dest, text, job.docx_include_image.then_some(job.image), job.dpi)?;
        }
    }
    Ok(())
}

pub fn encode_jpeg(img: &RgbImage, quality: u8) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    JpegEncoder::new_with_quality(&mut buf, quality.clamp(10, 100)).encode_image(img)?;
    Ok(buf)
}

/// Minimal single-page PDF embedding the image as JPEG.
pub fn image_pdf(img: &RgbImage, dpi: u32, quality: u8) -> Result<Vec<u8>> {
    let jpeg = encode_jpeg(img, quality)?;
    let (w, h) = img.dimensions();
    let pt = |px: u32| px as f64 * 72.0 / dpi.max(1) as f64;
    let (pw, ph) = (pt(w), pt(h));
    let content = format!("q {pw:.2} 0 0 {ph:.2} 0 0 cm /Im0 Do Q");

    let mut out: Vec<u8> = Vec::new();
    let mut offsets = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n");
    let mut obj = |out: &mut Vec<u8>, body: &[u8]| {
        offsets.push(out.len());
        out.extend_from_slice(body);
    };
    obj(&mut out, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    obj(&mut out, b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    obj(
        &mut out,
        format!(
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw:.2} {ph:.2}] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>\nendobj\n"
        )
        .as_bytes(),
    );
    let mut im = format!(
        "4 0 obj\n<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\nstream\n",
        jpeg.len()
    )
    .into_bytes();
    im.extend_from_slice(&jpeg);
    im.extend_from_slice(b"\nendstream\nendobj\n");
    obj(&mut out, &im);
    obj(
        &mut out,
        format!("5 0 obj\n<< /Length {} >>\nstream\n{content}\nendstream\nendobj\n", content.len()).as_bytes(),
    );
    let xref = out.len();
    write!(out, "xref\n0 6\n0000000000 65535 f \n")?;
    for o in &offsets {
        writeln!(out, "{o:010} 00000 n ")?;
    }
    write!(out, "trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n")?;
    Ok(out)
}

pub fn write_docx(dest: &Path, text: &str, image: Option<&RgbImage>, dpi: u32) -> Result<()> {
    use docx_rs::{Docx, Paragraph, Pic, Run};
    let mut doc = Docx::new();
    // Tesseract separates paragraphs with blank lines; lines inside a paragraph are joined.
    for para in text.split("\n\n") {
        let joined = para.lines().map(str::trim_end).collect::<Vec<_>>().join(" ");
        doc = doc.add_paragraph(Paragraph::new().add_run(Run::new().add_text(joined)));
    }
    if let Some(img) = image {
        let mut png = Cursor::new(Vec::new());
        img.write_to(&mut png, ImageFormat::Png)?;
        // EMU: 914400 per inch. Fit into a 16 cm wide column.
        let (w, h) = img.dimensions();
        let mut wemu = (w as f64 / dpi.max(1) as f64 * 914_400.0) as u32;
        let mut hemu = (h as f64 / dpi.max(1) as f64 * 914_400.0) as u32;
        let max_w = 16.0 / 2.54 * 914_400.0;
        if wemu as f64 > max_w {
            let s = max_w / wemu as f64;
            wemu = (wemu as f64 * s) as u32;
            hemu = (hemu as f64 * s) as u32;
        }
        let pic = Pic::new(&png.into_inner()).size(wemu, hemu);
        doc = doc.add_paragraph(Paragraph::new().add_run(Run::new().add_image(pic)));
    }
    let file = std::fs::File::create(dest).with_context(|| format!("creating {}", dest.display()))?;
    doc.build().pack(file)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docx_and_text_exports() {
        let dir = tempfile::tempdir().unwrap();
        let img = RgbImage::from_pixel(60, 30, image::Rgb([250, 250, 250]));
        let job = ExportJob {
            image: &img,
            text: Some("Bonjour\nle monde\n\nSecond paragraphe"),
            dpi: 300,
            jpeg_quality: 90,
            ocr: None,
            docx_include_image: true,
        };
        for f in [Format::Docx, Format::Txt, Format::Png, Format::Jpeg, Format::Tiff, Format::Bmp, Format::Pdf] {
            let p = dir.path().join(format!("out.{}", f.extension()));
            export(&job, f, &p).unwrap();
            assert!(std::fs::metadata(&p).unwrap().len() > 0, "{f:?}");
        }
        let docx = std::fs::read(dir.path().join("out.docx")).unwrap();
        assert!(docx.starts_with(b"PK"), "docx must be a zip archive");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("out.txt")).unwrap(),
            "Bonjour\nle monde\n\nSecond paragraphe"
        );
        // Searchable PDF without Tesseract must fail cleanly.
        assert!(export(&job, Format::SearchablePdf, &dir.path().join("x.pdf")).is_err());
    }

    #[test]
    fn tesseract_round_trip() {
        let Some(t) = crate::ocr::Tesseract::locate(None) else { return };
        let langs = t.languages().unwrap_or_default();
        let Some(lang) = ["fra", "eng", "afr"].into_iter().find(|l| langs.iter().any(|x| x == l)) else { return };
        // Render "HELLO 42" with a crude 5x7 bitmap font, scaled up.
        const GLYPHS: [(char, [u8; 7]); 6] = [
            ('H', [0x11, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11]),
            ('E', [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f]),
            ('L', [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1f]),
            ('O', [0x0e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e]),
            ('4', [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02]),
            ('2', [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f]),
        ];
        let text = "HELLO 42";
        let s = 6u32;
        let mut img = RgbImage::from_pixel((text.len() as u32 * 6 + 4) * s, 15 * s, image::Rgb([255, 255, 255]));
        for (i, ch) in text.chars().enumerate() {
            let Some((_, rows)) = GLYPHS.iter().find(|(c, _)| *c == ch) else { continue };
            for (ry, row) in rows.iter().enumerate() {
                for rx in 0..5 {
                    if row & (0x10 >> rx) != 0 {
                        for dy in 0..s {
                            for dx in 0..s {
                                img.put_pixel(
                                    (2 + i as u32 * 6 + rx) * s + dx,
                                    (4 + ry as u32) * s + dy,
                                    image::Rgb([0, 0, 0]),
                                );
                            }
                        }
                    }
                }
            }
        }
        let out = t.recognize(&img, lang, 300).unwrap();
        assert!(out.contains("HELLO") || out.contains("42"), "OCR output: {out:?}");
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("s.pdf");
        t.searchable_pdf(&img, lang, 300, &pdf).unwrap();
        assert!(std::fs::read(&pdf).unwrap().starts_with(b"%PDF"));
    }

    #[test]
    fn pdf_is_well_formed() {
        let img = RgbImage::from_pixel(40, 20, image::Rgb([200, 10, 10]));
        let pdf = image_pdf(&img, 300, 90).unwrap();
        assert!(pdf.starts_with(b"%PDF-1.4"));
        let find = |needle: &[u8]| pdf.windows(needle.len()).rposition(|w| w == needle);
        assert!(find(b"/Subtype /Image /Width 40 /Height 20").is_some());
        let sx = find(b"startxref\n").unwrap() + b"startxref\n".len();
        let tail = std::str::from_utf8(&pdf[sx..]).unwrap();
        let start: usize = tail.lines().next().unwrap().parse().unwrap();
        assert!(pdf[start..].starts_with(b"xref"));
        // Every object offset in the xref table must point at "<n> 0 obj".
        let table = std::str::from_utf8(&pdf[start..sx]).unwrap();
        for (i, line) in table.lines().skip(3).take(5).enumerate() {
            let off: usize = line[..10].parse().unwrap();
            assert!(pdf[off..].starts_with(format!("{} 0 obj", i + 1).as_bytes()), "object {}", i + 1);
        }
    }
}
