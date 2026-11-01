//! Helper executable: read-only probe by default, `serve` for the JSON-lines protocol.
fn main() -> std::process::ExitCode {
    bridge_calls_macos::cli::run(std::env::args().skip(1))
}
