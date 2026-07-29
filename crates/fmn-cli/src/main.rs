//! The `fmn` binary entry point.
#![forbid(unsafe_code)]

use std::io::Write as _;
use std::process::ExitCode;

fn main() -> ExitCode {
    let output = fmn_cli::run_os(std::env::args_os().skip(1));

    if !output.stdout.is_empty()
        && std::io::stdout()
            .lock()
            .write_all(output.stdout.as_bytes())
            .is_err()
    {
        return ExitCode::from(fmn_cli::internal_exit_code());
    }
    if !output.stderr.is_empty()
        && std::io::stderr()
            .lock()
            .write_all(output.stderr.as_bytes())
            .is_err()
    {
        return ExitCode::from(fmn_cli::internal_exit_code());
    }

    ExitCode::from(output.code)
}
