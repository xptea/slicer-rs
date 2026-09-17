//! Deterministic, headless reference compositor for P0 validation.
//!
//! This intentionally uses only `serde_json` and the standard library. It is
//! a small oracle for layer order, alpha compositing, clipping, and moving
//! synthetic shapes while the production Vulkan compositor is still under
//! validation.

use serde::Deserialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Debug, Deserialize)]
struct Scene {
    schema: String,
    scene_id: String,
    seed: u64,
    canvas: Canvas,
    frame_count: u32,
    fps: Rate,
    background_rgba: [u8; 4],
    unicode_text: String,
    layers: Vec<Layer>,
    pixel_checks: Vec<PixelCheck>,
}

#[derive(Debug, Deserialize)]
struct Canvas {
    width: u32,
    height: u32,
}

#[derive(Debug, Deserialize)]
struct Rate {
    numerator: u32,
    denominator: u32,
}

#[derive(Debug, Deserialize)]
struct Layer {
    id: String,
    z: i32,
    shape: String,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    rgba: [u8; 4],
    opacity: u8,
    dx_per_frame: i32,
    dy_per_frame: i32,
}

#[derive(Debug, Deserialize)]
struct PixelCheck {
    name: String,
    frame: u32,
    x: u32,
    y: u32,
    expected_rgba: [u8; 4],
}

#[derive(Debug)]
struct CheckResult {
    name: String,
    passed: bool,
    actual: [u8; 4],
    expected: [u8; 4],
}

fn main() {
    if let Err(error) = run() {
        eprintln!("compositor_bench: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut fixture = PathBuf::from("tests/fixtures/compositor_scene.json");
    let mut human = false;
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    let mut index = 0;
    while index < args.len() {
        match args[index].to_str() {
            Some("--fixture") => {
                index += 1;
                fixture = args
                    .get(index)
                    .ok_or_else(|| "--fixture requires a path".to_owned())?
                    .clone()
                    .into();
            }
            Some("--human") => human = true,
            Some("--help" | "-h") => {
                println!(
                    "Usage: compositor_bench [--fixture PATH] [--human]\n\nRenders the deterministic P0 scene and checks its reference pixels."
                );
                return Ok(());
            }
            Some(other) => return Err(format!("unknown argument {other}")),
            None => return Err("argument is not valid UTF-8".to_owned()),
        }
        index += 1;
    }

    let started = Instant::now();
    let bytes =
        fs::read(&fixture).map_err(|error| format!("read {}: {error}", fixture.display()))?;
    let scene: Scene = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {}: {error}", fixture.display()))?;
    if scene.canvas.width == 0 || scene.canvas.height == 0 || scene.frame_count == 0 {
        return Err("fixture canvas and frame count must be positive".to_owned());
    }
    let mut layers = scene.layers.iter().collect::<Vec<_>>();
    layers.sort_by_key(|layer| (layer.z, layer.id.as_str()));
    let mut hashes = Vec::with_capacity(scene.frame_count as usize);
    for frame_index in 0..scene.frame_count {
        let image = render_frame(&scene, &layers, frame_index)?;
        hashes.push(fnv1a64(&image));
    }

    let mut checks = Vec::with_capacity(scene.pixel_checks.len());
    for check in &scene.pixel_checks {
        if check.frame >= scene.frame_count
            || check.x >= scene.canvas.width
            || check.y >= scene.canvas.height
        {
            return Err(format!("pixel check {} is outside the fixture", check.name));
        }
        let image = render_frame(&scene, &layers, check.frame)?;
        let offset = ((check.y * scene.canvas.width + check.x) * 4) as usize;
        let actual = image[offset..offset + 4]
            .try_into()
            .expect("four pixel bytes");
        checks.push(CheckResult {
            name: check.name.clone(),
            passed: actual == check.expected_rgba,
            actual,
            expected: check.expected_rgba,
        });
    }
    let passed = checks.iter().all(|check| check.passed);
    let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
    if human {
        println!(
            "{} {}x{} {} frames at {}/{} fps in {:.2} ms",
            scene.scene_id,
            scene.canvas.width,
            scene.canvas.height,
            scene.frame_count,
            scene.fps.numerator,
            scene.fps.denominator,
            elapsed_ms
        );
        println!("unicode_text={}", scene.unicode_text);
        for check in &checks {
            println!(
                "{}: {} actual={:?} expected={:?}",
                check.name,
                if check.passed { "passed" } else { "failed" },
                check.actual,
                check.expected
            );
        }
        println!("status={}", if passed { "passed" } else { "failed" });
    } else {
        let checks = checks
            .iter()
            .map(|check| {
                serde_json::json!({
                    "name": check.name,
                    "status": if check.passed { "passed" } else { "failed" },
                    "actual_rgba": check.actual,
                    "expected_rgba": check.expected,
                })
            })
            .collect::<Vec<_>>();
        let report = serde_json::json!({
            "schema": "slicer.compositor-benchmark.v1",
            "fixture": fixture,
            "scene_schema": scene.schema,
            "scene_id": scene.scene_id,
            "seed": scene.seed,
            "canvas": { "width": scene.canvas.width, "height": scene.canvas.height },
            "frames": scene.frame_count,
            "fps": { "numerator": scene.fps.numerator, "denominator": scene.fps.denominator },
            "frame_hashes_fnv1a64": hashes.iter().map(|hash| format!("{hash:016x}")).collect::<Vec<_>>(),
            "checks": checks,
            "elapsed_ms": elapsed_ms,
            "status": if passed { "passed" } else { "failed" },
            "hardware": { "gpu": "unvalidated", "decoder": "synthetic" },
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
        );
    }
    if passed {
        Ok(())
    } else {
        Err("one or more reference pixel checks failed".to_owned())
    }
}

fn render_frame(scene: &Scene, layers: &[&Layer], frame: u32) -> Result<Vec<u8>, String> {
    let pixel_count = usize::try_from(scene.canvas.width)
        .ok()
        .and_then(|width| {
            usize::try_from(scene.canvas.height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or_else(|| "fixture dimensions overflow".to_owned())?;
    let mut output = Vec::with_capacity(pixel_count * 4);
    for _ in 0..pixel_count {
        output.extend_from_slice(&scene.background_rgba);
    }
    for layer in layers {
        let x =
            layer.x + layer.dx_per_frame * i32::try_from(frame).map_err(|_| "frame overflow")?;
        let y =
            layer.y + layer.dy_per_frame * i32::try_from(frame).map_err(|_| "frame overflow")?;
        if layer.width <= 0 || layer.height <= 0 {
            continue;
        }
        let left = x.max(0) as u32;
        let top = y.max(0) as u32;
        let right = (x.saturating_add(layer.width))
            .min(scene.canvas.width as i32)
            .max(0) as u32;
        let bottom = (y.saturating_add(layer.height))
            .min(scene.canvas.height as i32)
            .max(0) as u32;
        for py in top.min(scene.canvas.height)..bottom.min(scene.canvas.height) {
            for px in left.min(scene.canvas.width)..right.min(scene.canvas.width) {
                let inside = match layer.shape.as_str() {
                    "rect" => true,
                    "circle" => {
                        let local_x = (px as f64 + 0.5 - f64::from(x)) / f64::from(layer.width);
                        let local_y = (py as f64 + 0.5 - f64::from(y)) / f64::from(layer.height);
                        let dx = local_x - 0.5;
                        let dy = local_y - 0.5;
                        dx * dx + dy * dy <= 0.25
                    }
                    _ => return Err(format!("unsupported fixture shape {}", layer.shape)),
                };
                if inside {
                    let offset = ((py * scene.canvas.width + px) * 4) as usize;
                    blend(&mut output[offset..offset + 4], layer.rgba, layer.opacity);
                }
            }
        }
    }
    Ok(output)
}

fn blend(destination: &mut [u8], source: [u8; 4], opacity: u8) {
    let alpha = f64::from(source[3]) / 255.0 * f64::from(opacity) / 255.0;
    if alpha <= 0.0 {
        return;
    }
    if alpha >= 1.0 {
        destination.copy_from_slice(&source);
        return;
    }
    let destination_alpha = f64::from(destination[3]) / 255.0;
    let output_alpha = alpha + destination_alpha * (1.0 - alpha);
    if output_alpha <= f64::EPSILON {
        destination.fill(0);
        return;
    }
    for channel in 0..3 {
        let value = (f64::from(source[channel]) * alpha
            + f64::from(destination[channel]) * destination_alpha * (1.0 - alpha))
            / output_alpha;
        destination[channel] = value.round().clamp(0.0, 255.0) as u8;
    }
    destination[3] = (output_alpha * 255.0).round().clamp(0.0, 255.0) as u8;
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[allow(dead_code)]
fn fixture_exists(path: &Path) -> bool {
    path.is_file()
}
