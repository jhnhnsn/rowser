use std::process::ExitCode;

fn main() -> ExitCode {
    let command = match rowser::cli::parse_from(std::env::args_os()) {
        Ok(command) => command,
        Err(err) => err.exit(),
    };
    eprintln!("rowser {}: not implemented yet", command.name());
    ExitCode::from(2)
}
