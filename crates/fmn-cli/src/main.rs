//! The `fmn` binary entry point.
#![forbid(unsafe_code)]

fn main() {
    // The manim-compatible flag surface lands with W9 (§13.6).
    eprintln!(
        "fmn {}: engine under construction; the CLI surface lands with Proscenium (W9)",
        env!("CARGO_PKG_VERSION")
    );
    std::process::exit(2);
}
