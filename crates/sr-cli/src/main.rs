fn main() {
    if let Err(e) = sr_cli::run_from(std::env::args_os()) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
