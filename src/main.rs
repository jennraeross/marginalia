use marginalia::app::MarginaliaApp;
use marginalia::library::Library;

fn main() -> eframe::Result<()> {
    let root = Library::default_root();
    let lib = Library::open(root).expect("open library");
    let mut viewport = egui::ViewportBuilder::default().with_inner_size([1100.0, 750.0]);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-512.png")) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "marginalia",
        options,
        Box::new(|cc| Ok(Box::new(MarginaliaApp::new(lib, cc)))),
    )
}
