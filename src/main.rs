use std::process::ExitCode;

fn main() -> ExitCode {
    let command = match rowser::cli::parse_from(std::env::args_os()) {
        Ok(command) => command,
        Err(err) => err.exit(),
    };
    if let rowser::cli::Command::Daemon {
        action: None,
        runtime_dir,
        open_command,
    } = command
    {
        return rowser::daemon::run(runtime_dir, open_command);
    }
    eprintln!("rowser {}: not implemented yet", command.label());
    ExitCode::from(2)
}
