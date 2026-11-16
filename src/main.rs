#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod core;
mod formatter;
mod history;
mod lexicon;
mod meter;
mod storage;
mod stress;
mod ui;
mod updater;

fn main() -> eframe::Result {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon/poetrhymer.png"))
        .expect("Invalid embedded application icon");
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Poetrhymer · редактор стихов")
            .with_icon(icon)
            .with_inner_size([1320.0, 860.0])
            .with_min_inner_size([980.0, 760.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Poetrhymer",
        options,
        Box::new(|cc| Ok(Box::new(ui::PoetApp::new(cc)))),
    )
}
