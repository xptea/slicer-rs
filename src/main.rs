use anyhow::{Context, Result, bail};
#[cfg(feature = "desktop")]
use slicer::home;
use slicer::{job, media, preview, waveform};
#[cfg(feature = "desktop")]
mod ui;

fn main() {
    if let Err(error) = run() {
        eprintln!("Slicer: {error:#}");
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.is_empty() || (args.len() == 2 && args[0] == "open") {
        // Native video currently embeds an X11 drawable. Use XWayland on Wayland
        // desktops, selected before GPUI or any worker threads are initialized.
        #[cfg(all(feature = "desktop", target_os = "linux"))]
        if std::env::var_os("DISPLAY").is_some_and(|value| !value.is_empty())
            && std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty())
        {
            use std::os::unix::process::CommandExt;
            let error = std::process::Command::new(std::env::current_exe()?)
                .args(&args)
                .env_remove("WAYLAND_DISPLAY")
                .exec();
            return Err(error).context("Could not start the native XWayland video window");
        }
        #[cfg(feature = "desktop")]
        {
            ui::run(args.get(1).map(std::path::PathBuf::from));
            return Ok(());
        }
        #[cfg(not(feature = "desktop"))]
        bail!("Desktop support disabled. Use inspect or export.");
    }
    match args[0].to_str() {
        Some("--help" | "-h") => println!(
            "Slicer\n  slicer                  Open the desktop app\n  slicer open INPUT       Open a file in the editor\n  slicer inspect INPUT    Inspect media using bundled ffprobe\n  slicer export INPUT OUTPUT START END [fast|exact]\n  slicer preview INPUT SECONDS OUTPUT.png\n  slicer binaries         Print resolved bundled tool paths\n\nExport output uses the destination extension (.mp4, .mkv, .webm, .mp3, .wav, or .gif).\nExports never replace existing files. Times are seconds.\nSLICER_FFMPEG_DIR explicitly overrides bundled tools for development."
        ),
        Some("binaries") => {
            let bins = media::Binaries::resolve()?;
            println!(
                "ffmpeg={}\nffprobe={}",
                bins.ffmpeg.display(),
                bins.ffprobe.display()
            );
        }
        Some("inspect") if args.len() == 2 => {
            let info =
                media::inspect(&media::Binaries::resolve()?, std::path::Path::new(&args[1]))?;
            println!("Duration: {:.3}s\nSize: {} bytes", info.duration, info.size);
            for stream in info.streams {
                println!(
                    "Track {}: {} {} {}x{} channels={}",
                    stream.index,
                    stream.kind,
                    stream.codec,
                    stream.width.unwrap_or(0),
                    stream.height.unwrap_or(0),
                    stream.channels.unwrap_or(0)
                );
            }
        }
        Some("export") if (5..=6).contains(&args.len()) => {
            let parse = |i: usize| -> Result<f64> {
                args[i]
                    .to_str()
                    .context("Time is not valid text")?
                    .parse()
                    .context("Time must be a number in seconds")
            };
            let output = std::path::PathBuf::from(&args[2]);
            let format = match output.extension().and_then(|x| x.to_str()) {
                Some("mp4") => job::OutputFormat::Mp4,
                Some("mkv") => job::OutputFormat::Mkv,
                Some("webm") => job::OutputFormat::Webm,
                Some("mp3") => job::OutputFormat::Mp3,
                Some("wav") => job::OutputFormat::Wav,
                Some("gif") => job::OutputFormat::Gif,
                _ => bail!("Use .mp4, .mkv, .webm, .mp3, .wav or .gif output"),
            };
            let mode = match args.get(5).and_then(|a| a.to_str()).unwrap_or("exact") {
                "fast" => job::TrimMode::Fast,
                "exact" => job::TrimMode::Exact,
                _ => bail!("Mode must be fast or exact"),
            };
            let handle = job::JobHandle::spawn(
                media::Binaries::resolve()?,
                job::ExportRequest {
                    input: args[1].clone().into(),
                    output,
                    start: parse(3)?,
                    end: parse(4)?,
                    mode,
                    format,
                    crop: None,
                    quality: 75,
                    mute_audio: false,
                },
            )?;
            loop {
                match handle.events.recv().context("Export worker stopped")? {
                    job::JobEvent::Progress(progress) => {
                        eprintln!("Progress: {:.1}%", progress * 100.0)
                    }
                    job::JobEvent::Completed(path) => {
                        println!("Saved {}", path.display());
                        break;
                    }
                    job::JobEvent::Cancelled => bail!("Export cancelled"),
                    job::JobEvent::Failed(error) => bail!(error),
                }
            }
        }
        Some("preview") if args.len() == 4 => {
            let worker = preview::PreviewWorker::new(media::Binaries::resolve()?);
            worker.request(
                args[1].clone().into(),
                args[2].to_str().context("Invalid time")?.parse()?,
            );
            let bytes = worker.events.recv()?.result.map_err(anyhow::Error::msg)?;
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&args[3])?;
            file.write_all(&bytes)?;
        }
        _ => bail!("Unknown command or arguments. Run slicer --help."),
    }
    Ok(())
}
