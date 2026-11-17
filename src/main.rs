use std::process::ExitCode;

fn main() -> ExitCode {
    match rowser::cli::parse_from(std::env::args_os()) {
        Ok(command) => rowser::cli::run(command),
        Err(err) => err.exit(),
    }
}
