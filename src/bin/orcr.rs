fn main() {
    if let Err(error) = orchestratr::cli::run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}
