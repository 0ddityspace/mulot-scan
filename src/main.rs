#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
// Per-channel loops (`for k in 0..3`) read better than iterator chains for pixel math.
#![allow(clippy::needless_range_loop)]

mod app;
mod config;
mod device;
mod edit;
mod export;
mod ocr;
mod stitch;
mod worker;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use eframe::egui;

use device::{DeviceEvent, ImagingSettings, ReplaySource, ScanSession, Scanner};

const USAGE: &str = "\
mulot-scan — logiciel pour souris scanner, compatible IRIScan Mouse

Utilisation :
  mulot-scan                         lance l'interface graphique
  mulot-scan --replay <flux.mjpg>    interface graphique alimentée par un enregistrement
  mulot-scan probe                   affiche les informations de la souris
  mulot-scan reset                   remet la souris en mode souris normal
  mulot-scan record <flux.mjpg> [s]  enregistre le flux brut pendant s secondes (défaut 10)
  mulot-scan stitch <flux.mjpg> <sortie.png>
                                    assemble un enregistrement hors ligne
";

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => run_gui(None),
        Some("--replay") => {
            let p = args.get(1).context("--replay needs a file")?;
            run_gui(Some(PathBuf::from(p)))
        }
        Some("probe") => probe(),
        Some("reset") => {
            let s = Scanner::open()?;
            s.mcu().enable_mouse()?;
            println!("mode souris restauré ({:#x})", s.mcu().current_mode()?);
            Ok(())
        }
        Some("record") => {
            let out = PathBuf::from(args.get(1).context("record needs an output file")?);
            let secs: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10.0);
            record(&out, secs)
        }
        Some("stitch") => {
            let input = PathBuf::from(args.get(1).context("stitch needs an input file")?);
            let output = PathBuf::from(args.get(2).context("stitch needs an output file")?);
            stitch_offline(&input, &output)
        }
        Some("-h" | "--help" | "help") => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => bail!("argument inconnu : {other}\n\n{USAGE}"),
    }
}

fn run_gui(replay: Option<PathBuf>) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Mulot-scan")
            .with_app_id("mulot-scan")
            .with_icon(window_icon())
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native("mulot-scan", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, replay)))))
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

fn probe() -> Result<()> {
    let s = Scanner::open()?;
    println!("périphérique : {}", s.description());
    println!("informations firmware : {:02x?}", s.info.raw);
    let m = s.mcu();
    println!("mode : {:#x}  bouton : {}", m.current_mode()?, m.button_pressed()?);
    println!("exposition : {:.3} ms", m.exposure_ms()?);
    m.disable_mouse()?;
    let id = m.read_mouse_id();
    let user = m.read_user_data();
    m.enable_mouse()?;
    if let Ok(id) = id {
        println!("identifiant : {:02x?}", &id[..id.iter().position(|&b| b == 0xff).unwrap_or(id.len())]);
    }
    match m.security_check(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1),
    ) {
        Ok(ok) => println!("authentification de la souris : {}", if ok { "réussie" } else { "échouée" }),
        Err(e) => println!("authentification impossible : {e:#}"),
    }
    match user {
        Ok(Some(d)) => println!("données de calibrage d'usine : {} octets", d.len()),
        Ok(None) => println!("pas de données de calibrage d'usine"),
        Err(e) => println!("lecture du calibrage impossible : {e:#}"),
    }
    Ok(())
}

fn record(out: &Path, secs: f64) -> Result<()> {
    let s = Arc::new(Scanner::open()?);
    let settings = config::Config::load().imaging;
    let session = s.start(settings, Some(out))?;
    println!("enregistrement de {secs} s dans {} — déplacez la souris…", out.display());
    let n = drain(session, secs, true);
    println!("{n} images, mode souris restauré ({:#x})", s.mcu().current_mode()?);
    let _ = ImagingSettings::default();
    Ok(())
}

fn drain(mut session: ScanSession, secs: f64, verbose: bool) -> usize {
    let t0 = Instant::now();
    let mut n = 0;
    while t0.elapsed().as_secs_f64() < secs {
        match session.events.recv_timeout(Duration::from_millis(500)) {
            Ok(DeviceEvent::Frame(f)) => {
                n += 1;
                if verbose && n % 15 == 1 {
                    println!(
                        "#{:4} cnt {:3} mots {:?} qualité {:?} extra {} flags {:#x}",
                        f.index, f.data.counter, f.data.words, f.data.surface_quality, f.data.extra, f.data.flags
                    );
                }
            }
            Ok(DeviceEvent::Button(b)) => println!("bouton : {b}"),
            Ok(DeviceEvent::Error(e)) => println!("erreur : {e}"),
            Ok(DeviceEvent::Stopped) => break,
            Err(_) => {}
        }
    }
    session.stop();
    n
}

fn stitch_offline(input: &Path, output: &Path) -> Result<()> {
    let cfg = config::Config::load();
    let mut st = stitch::Stitcher::new(cfg.stitch);
    st.flat = config::load_flat_field().ok().flatten();
    let src = ReplaySource::open(input, 10_000.0)?;
    let total = src.len();
    let session = ScanSession::spawn(Box::new(src), None, None)?;
    let t0 = Instant::now();
    let mut lost = 0;
    loop {
        match session.events.recv() {
            Ok(DeviceEvent::Frame(f)) => {
                let info = st.push(&f);
                if info.lost {
                    lost += 1;
                }
            }
            Ok(DeviceEvent::Stopped) | Err(_) => break,
            Ok(_) => {}
        }
    }
    let img = st.finish(cfg.auto_levels).context("nothing stitched")?;
    img.save(output)?;
    println!(
        "{} images sur {total} assemblées en {:.1} s ({lost} sans recalage) → {} ({}×{})",
        st.frame_count(),
        t0.elapsed().as_secs_f64(),
        output.display(),
        img.width(),
        img.height()
    );
    Ok(())
}
