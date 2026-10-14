//! Outlook RS: a clean-room re-creation of Outlook Classic's mail experience in Rust.
//!
//! Usage: `outlook-rs [--demo] [--screenshot <file.png> [--compose]]`

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod demo;
mod html;
mod mime;
mod model;
mod oauth;
mod store;
mod theme;
mod ui;
#[cfg(windows)]
mod webview;
mod worker;

use std::path::{Path, PathBuf};

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let demo = args.iter().any(|a| a == "--demo");
    let screenshot = args.iter().position(|a| a == "--screenshot").and_then(|i| args.get(i + 1)).map(|p| app::Screenshot {
        path: PathBuf::from(p),
        compose: args.iter().any(|a| a == "--compose"),
    });

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Outlook RS")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native("Outlook RS", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, demo, screenshot)))))
}

/// Save an egui screenshot as PNG (used by `--screenshot`).
pub fn save_png(path: &Path, image: &eframe::egui::ColorImage) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let [w, h] = image.size;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    let bytes: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
    writer.write_image_data(&bytes).map_err(|e| e.to_string())
}
