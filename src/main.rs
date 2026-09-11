fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("scan") => glomeris::scanner::run_scan_cli(&args[2..]),
        _ => println!("glomeris {}", env!("CARGO_PKG_VERSION")),
    }
}
