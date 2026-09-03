fn main() {
    if let Err(error) = r_corpus::worker::run_stdio() {
        eprintln!("r-parse-worker I/O failure: {error}");
        std::process::exit(1);
    }
}
