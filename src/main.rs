use marginalia::app::MarginaliaApp;
use marginalia::library::Library;

fn main() -> eframe::Result<()> {
    let root = Library::default_root();
    let lib = Library::open(root).expect("open library");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 750.0]),
        ..Default::default()
    };
    eframe::run_native(
        "marginalia",
        options,
        Box::new(|cc| Ok(Box::new(MarginaliaApp::new(lib, cc)))),
    )
}
