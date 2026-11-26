use std::process::ExitCode;

use clap::Parser;

fn main() -> ExitCode {
    let cli = kitbag::cli::Cli::parse_from(kitbag::cli::normalize_args(std::env::args_os()));
    match kitbag::cli::run(cli) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(e) => {
            kitbag::cli::report_error(&e);
            ExitCode::from(2)
        }
    }
}
