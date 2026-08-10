//! The `fmn` binary entry point.
#![forbid(unsafe_code)]

use std::io::{Read as _, Write as _};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn main() -> ExitCode {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let output = if fmn_cli::is_internal_studio_worker_os(&args) {
        fmn_cli::run_internal_studio_worker_os(&args)
    } else if fmn_cli::is_studio_invocation_os(&args) {
        let shutdown = Arc::new(AtomicBool::new(false));
        let watcher_shutdown = Arc::clone(&shutdown);
        let watcher = std::thread::Builder::new()
            .name("fmn-studio-stdin".to_owned())
            .spawn(move || {
                let mut input = std::io::stdin().lock();
                let mut discard = [0_u8; 1024];
                while input.read(&mut discard).is_ok_and(|read| read != 0) {}
                watcher_shutdown.store(true, Ordering::Release);
            });
        if let Err(error) = watcher {
            fmn_cli::RunOutput {
                code: fmn_cli::internal_exit_code(),
                stdout: String::new(),
                stderr: format!("fmn: cannot start Studio shutdown watcher: {error}\n"),
            }
        } else {
            let stdout = std::io::stdout();
            fmn_cli::run_studio_os(&args, &mut stdout.lock(), &shutdown)
        }
    } else {
        fmn_cli::run_os(args)
    };

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
