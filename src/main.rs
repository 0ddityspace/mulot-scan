#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
// Per-channel loops (`for k in 0..3`) read better than iterator chains for pixel math.
#![allow(clippy::needless_range_loop)]

mod app;
mod autostart;
mod cli;
mod config;
mod device;
mod edit;
mod export;
mod ocr;
mod stitch;
mod worker;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use eframe::egui;

use device::Scanner;

const USAGE: &str = "\
mulot-scan — logiciel pour souris scanner, compatible IRIScan Mouse

Utilisation :
  mulot-scan                         lance l'interface graphique
  mulot-scan --replay <flux.mjpg>    interface graphique alimentée par un enregistrement
  mulot-scan --record <dossier>      interface graphique ; chaque scan est aussi
                                    enregistré en flux brut dans le dossier
  mulot-scan --tray                  démarre caché dans la zone de notification ; le
                                    bouton Scan de la souris ouvre la fenêtre
  mulot-scan probe                   affiche les informations de la souris
  mulot-scan reset                   remet la souris en mode souris normal
  mulot-scan record <flux.mjpg> [s]  enregistre le flux brut pendant s secondes (défaut 10)
  mulot-scan stitch <flux.mjpg> <sortie.png> [trace.csv]
                                    assemble un enregistrement hors ligne ; trace.csv
                                    reçoit la position estimée et les capteurs par image
";

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => run_gui(app::GuiOptions::default()),
        Some(autostart::HIDDEN_FLAG) => run_gui(app::GuiOptions { start_hidden: true, ..Default::default() }),
        Some("--replay") => {
            let p = args.get(1).context("--replay needs a file")?;
            run_gui(app::GuiOptions { replay: Some(PathBuf::from(p)), ..Default::default() })
        }
        Some("--record") => {
            let dir = PathBuf::from(args.get(1).context("--record needs a directory")?);
            std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
            run_gui(app::GuiOptions { record_dir: Some(dir), ..Default::default() })
        }
        Some("probe") => cli::probe(),
        Some("reset") => {
            let s = Scanner::open()?;
            s.mcu().enable_mouse()?;
            println!("mode souris restauré ({:#x})", s.mcu().current_mode()?);
            Ok(())
        }
        Some("record") => {
            let out = PathBuf::from(args.get(1).context("record needs an output file")?);
            let secs: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10.0);
            cli::record(&out, secs)
        }
        Some("stitch") => {
            let input = PathBuf::from(args.get(1).context("stitch needs an input file")?);
            let output = PathBuf::from(args.get(2).context("stitch needs an output file")?);
            let trace = args.get(3).map(PathBuf::from);
            cli::stitch_offline(&input, &output, trace.as_deref())
        }
        Some("-h" | "--help" | "help") => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => bail!("argument inconnu : {other}\n\n{USAGE}"),
    }
}

fn run_gui(opts: app::GuiOptions) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_visible(!opts.start_hidden)
            .with_title("Mulot-scan")
            .with_app_id("mulot-scan")
            .with_icon(window_icon())
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native("mulot-scan", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, opts)))))
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Application icon (also embedded in the Windows executable by `build.rs`).
fn window_icon() -> egui::IconData {
    let img = image::load_from_memory_with_format(include_bytes!("../assets/icon-256.png"), image::ImageFormat::Png)
        .expect("embedded icon")
        .to_rgba8();
    let (width, height) = img.dimensions();
    egui::IconData { rgba: img.into_raw(), width, height }
}
