//! Prints the hardware report of this PC (read-only):
//!
//! `cargo run -p zest-hardware --example hardware_report [-- --json]`

fn main() {
    let started = std::time::Instant::now();
    let inventory = zest_hardware::inventory();
    let elapsed = started.elapsed();
    if std::env::args().any(|arg| arg == "--json") {
        println!("{}", zest_hardware::to_json(&inventory));
    } else {
        print!("{}", zest_hardware::to_text(&inventory));
        println!("\n(collected in {} ms)", elapsed.as_millis());
    }
}
