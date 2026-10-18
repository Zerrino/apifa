#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use apifa::{app, cli};

fn main() -> eframe::Result<()> {
    // Old macOS versions pass `-psn_…` to apps opened from the Finder.
    let args: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("-psn_")).collect();
    if let Some(code) = cli::main(&args) {
        std::process::exit(code);
    }
    let mut viewport = egui::ViewportBuilder::default();
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")) {
        viewport = viewport.with_icon(icon);
    }
    // Our own title bar replaces the system one. On macOS the window keeps its
    // (invisible) system title bar: without it there is no native full screen, which hides
    // the menu bar and the Dock.
    #[cfg(target_os = "macos")]
    let viewport = viewport.with_fullsize_content_view(true).with_titlebar_shown(false).with_title_shown(false).with_titlebar_buttons_shown(false);
    #[cfg(not(target_os = "macos"))]
    let viewport = viewport.with_decorations(false);
    let options = eframe::NativeOptions {
        viewport: viewport.with_title("apifa").with_app_id("apifa").with_inner_size([1440.0, 900.0]).with_min_inner_size([960.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native("apifa", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
