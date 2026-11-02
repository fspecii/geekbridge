//! Writes the tray icon set from the same dot tables the service draws at run time.
//!
//!   cargo run -p bridge-service --example tray-icons -- svg DIR          # icon-<state>-<style>.svg
//!   cargo run -p bridge-service --example tray-icons -- png DIR [SIZE]   # the service's own renderer
//!
//! `scripts/gen-tray-icons.sh` runs the first form and rasterises the SVGs with rsvg-convert.
use bridge_service::tray::{
    icon::{self, Style},
    model::IconState,
};
use std::{fs, path::PathBuf, process::ExitCode};

const STYLES: [(&str, Style); 3] = [
    ("mono", Style::Light),
    ("mono-dark", Style::Dark),
    ("tile", Style::Tile),
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(kind), Some(dir)) = (args.first(), args.get(1)) else {
        eprintln!("usage: tray-icons svg|png DIR [SIZE]");
        return ExitCode::from(2);
    };
    let dir = PathBuf::from(dir);
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("{e}");
        return ExitCode::FAILURE;
    }
    let size: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(48);
    for state in IconState::ALL {
        for (name, style) in STYLES {
            let (file, bytes) = match kind.as_str() {
                "svg" => (
                    format!("icon-{}-{name}.svg", state.slug()),
                    icon::svg(state, style).into_bytes(),
                ),
                "png" => (
                    format!("icon-{}-{name}-{size}.png", state.slug()),
                    icon::render(state, size, style).to_png(),
                ),
                _ => {
                    eprintln!("unknown kind {kind}");
                    return ExitCode::from(2);
                }
            };
            if let Err(e) = fs::write(dir.join(&file), bytes) {
                eprintln!("{file}: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}
