fn main() {
    let code = match xtcc::cli::run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("xtcc: {error:#}");
            1
        }
    };
    std::process::exit(code);
}
