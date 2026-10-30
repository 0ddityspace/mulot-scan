//! Command-line subcommands (`probe`, `record`, `stitch`).

use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::device::{DeviceEvent, ReplaySource, ScanSession, Scanner};
use crate::{config, stitch};

pub fn probe() -> Result<()> {
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

pub fn record(out: &Path, secs: f64) -> Result<()> {
    let s = Arc::new(Scanner::open()?);
    let settings = config::Config::load().imaging;
    let session = s.start(settings, Some(out))?;
    println!("enregistrement de {secs} s dans {} — déplacez la souris…", out.display());
    let n = drain(session, secs, true);
    println!("{n} images, mode souris restauré ({:#x})", s.mcu().current_mode()?);
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

/// Assembles a recording. With `trace`, also writes one CSV line per frame
/// (estimated pose, tracking state and raw sensor data) for diagnostics.
pub fn stitch_offline(input: &Path, output: &Path, trace: Option<&Path>) -> Result<()> {
    let cfg = config::Config::load();
    let mut st = stitch::Stitcher::new(cfg.stitch);
    st.flat = config::load_flat_field().ok().flatten();
    let src = ReplaySource::open(input, 10_000.0)?;
    let total = src.len();
    let session = ScanSession::spawn(Box::new(src), None, None)?;
    let t0 = Instant::now();
    let mut lost = 0;
    let mut csv = match trace {
        Some(p) => {
            let mut w =
                std::io::BufWriter::new(std::fs::File::create(p).with_context(|| format!("creating {}", p.display()))?);
            writeln!(
                w,
                "frame,x,y,theta,confidence,speed,lost,lifted,searching,laser_only,\
                 w0,w1,w2,w3,w4,w5,quality0,quality1,extra,flags,dropped"
            )?;
            Some(w)
        }
        None => None,
    };
    loop {
        match session.events.recv() {
            Ok(DeviceEvent::Frame(f)) => {
                let info = st.push(&f);
                if info.lost {
                    lost += 1;
                }
                if let Some(w) = csv.as_mut() {
                    let (p, d) = (info.pose, &f.data);
                    let words = d.words.map(|v| (v as i16).to_string()).join(",");
                    writeln!(
                        w,
                        "{},{:.2},{:.2},{:.5},{:.1},{:.1},{},{},{},{},{words},{},{},{},{},{}",
                        f.index,
                        p.x,
                        p.y,
                        p.theta,
                        info.confidence,
                        info.speed,
                        info.lost as u8,
                        info.lifted as u8,
                        info.searching as u8,
                        info.laser_only as u8,
                        d.surface_quality[0],
                        d.surface_quality[1],
                        d.extra,
                        d.flags,
                        d.frame_dropped as u8
                    )?;
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
