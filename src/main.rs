#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod core;
mod lexicon;
mod storage;
mod ui;
mod updater;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Poetrhymer · редактор стихов")
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
