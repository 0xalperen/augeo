//! augeo composition root. Later milestones wire the TUI, engine and system adapters here.

#![forbid(unsafe_code)]

fn main() {
    println!("augeo {}", env!("CARGO_PKG_VERSION"));
}
