//! Icons of the interface: a subset of the Phosphor icon font (MIT licence,
//! <https://phosphoricons.com>, `assets/fonts/`), mixed into ordinary text.
//!
//! After adding an icon here, regenerate the subset with
//! `scripts/subset-icons.sh`.

use eframe::egui;

pub const SCAN: &str = "\u{EBB6}";
pub const STOP: &str = "\u{E46E}";
pub const FOLDER_OPEN: &str = "\u{E256}";
pub const SETTINGS: &str = "\u{E272}";
pub const INFO: &str = "\u{E2CE}";
pub const TURN_LEFT: &str = "\u{E038}";
pub const TURN_RIGHT: &str = "\u{E036}";
pub const MIRROR: &str = "\u{ED6A}";
pub const CROP: &str = "\u{E1D4}";
pub const MAGIC: &str = "\u{E6B6}";
pub const ANGLE: &str = "\u{E7BC}";
pub const ERASER: &str = "\u{E21E}";
pub const IMAGE: &str = "\u{E2CA}";
pub const PALETTE: &str = "\u{E6C8}";
pub const TEXT: &str = "\u{E6EE}";
pub const COPY: &str = "\u{E1CA}";
pub const SAVE: &str = "\u{E248}";
pub const UNDO: &str = "\u{E08A}";
pub const REDO: &str = "\u{E08C}";
pub const HAND: &str = "\u{E298}";
pub const ZOOM_OUT: &str = "\u{E30E}";
pub const ZOOM_IN: &str = "\u{E310}";
pub const FIT: &str = "\u{E1D0}";
pub const FULL_FRAME: &str = "\u{E626}";
pub const CHECK: &str = "\u{E182}";
pub const CANCEL: &str = "\u{E4F6}";
pub const TRASH: &str = "\u{E4A6}";
pub const RESET: &str = "\u{E096}";
pub const CALIBRATE: &str = "\u{E568}";
pub const FILE_IMAGE: &str = "\u{EA24}";
pub const FILE_PDF: &str = "\u{E702}";
pub const FILE_TEXT: &str = "\u{E23A}";
pub const FILE_DOC: &str = "\u{EB1E}";
pub const MOUSE: &str = "\u{E644}";
pub const WARNING: &str = "\u{E4E0}";
pub const OCR: &str = "\u{E238}";

/// Adds the icon font after the text font, so that icons can be written in
/// any label.
pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("icons".into(), egui::FontData::from_static(include_bytes!("../../assets/fonts/icons.ttf")).into());
    if let Some(keys) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        let at = keys.len().min(1);
        keys.insert(at, "icons".into());
    }
    ctx.set_fonts(fonts);
}

/// "icon  label".
pub fn label(icon: &str, text: &str) -> String {
    format!("{icon}  {text}")
}
