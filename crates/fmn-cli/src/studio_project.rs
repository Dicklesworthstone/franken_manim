//! Native Cargo-project front door. This host never executes scene callbacks.
//! Manim's generated flag parser continues to own the ordinary `studio` route.

mod reload;

use std::error::Error;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use fmn_cache::{NamespacePolicy, Store, StoreConfig};
use fmn_platform::clock::{Clock, StdClock};
use fmn_platform::entropy::{HostEntropy, StdHostEntropy};
use fmn_platform::fs::{FileSystem, VirtualFs};
use fmn_platform::process::StdProcessRunner;
use fmn_scene::Journal;
use fmn_studio::project::{CargoBuildConfig, CargoRebuildDriver, CargoTarget};
use fmn_studio::project_watch::{SourceWatch, WatchFilter, WatchLimits};
use fmn_studio::protocol::studio_seek_command;
use fmn_studio::{
    CapabilityToken, FrameHub, ProtocolLimits, StdWorkerLauncher, StudioHost, StudioHostConfig,
    StudioWorkerSession, Supervisor, SupervisorConfig, SupervisorReply, SupervisorRequest,
    WorkerResponse,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const HELP: &str = "Usage: fmn studio-project --manifest-path PATH --cargo ABSOLUTE_PATH \\\n         --package NAME (--bin NAME | --example NAME) --scene NAME [OPTIONS]\n\n\
Build and preview a trusted native Rust Studio worker in the main fmn binary.\n\
PATH may be a Cargo.toml or a project directory. Select nightly Cargo beside\n\
its rustc, for example with `rustup which cargo`. The selected executable must\n\
serve the native Studio worker protocol; arbitrary render binaries are not workers.\n\n\
Options:\n\
  --worker-arg VALUE       Repeat for each worker-mode argument (not Cargo flags)\n\
  --feature NAME          Repeat for each selected Cargo feature\n\
  --release               Build the worker with Cargo's release profile\n\
  --allow-network         Permit Cargo dependency downloads (offline by default)\n\
  --build-timeout SECONDS  Positive compiler deadline (default: 300)\n\
  --help, -h              Show this help without building or opening a listener\n\n\
Builds are locked and bounded. Source changes and Browser Restart recompile\n\
and restore the committed scene through the existing journal. Failed builds\n\
retain the last healthy worker. Temporary watch/connection failures recover\n\
without restarting Studio. The browser uses a loopback capability URL.\n\
Close stdin to stop; pending rebuild I/O is cancelled during shutdown.\n";

#[derive(Debug)]
struct Options {
    manifest: PathBuf,
    cargo: PathBuf,
    package: String,
    target: CargoTarget,
    scene: String,
    worker_args: Vec<String>,
    features: Vec<String>,
    release: bool,
    offline: bool,
    timeout: Duration,
}

pub(super) fn selected(args: &[OsString]) -> bool {
    args.first().is_some_and(|arg| arg == "studio-project")
}

fn value<'a>(args: &'a [String], index: &mut usize, flag: &str) -> Result<&'a str> {
    *index += 1;
    let value = args.get(*index).ok_or_else(|| format!("{flag} requires a value"))?;
    if value.is_empty() || value.contains('\0') || value.len() > 8192 {
        return Err(format!("{flag} has an empty, NUL-containing or oversized value").into());
    }
    Ok(value)
}

fn once<T>(slot: &mut Option<T>, item: T, flag: &str) -> Result<()> {
    if slot.is_some() {
        return Err(format!("{flag} may be supplied only once").into());
    }
    *slot = Some(item);
    Ok(())
}

fn parse(args: &[OsString]) -> Result<Option<Options>> {
    if !selected(args) || args.len() > 512 {
        return Err("invalid native-project invocation or too many arguments".into());
    }
    let args = args.iter().map(|arg| {
        arg.to_str().map(str::to_owned).ok_or("native-project arguments must be UTF-8")
    }).collect::<std::result::Result<Vec<_>, _>>()?;
    let (mut manifest, mut cargo, mut package, mut target, mut scene) = (None, None, None, None, None);
    let (mut worker_args, mut features) = (Vec::new(), Vec::new());
    let (mut release, mut allow_network, mut timeout) = (false, false, None);
    let mut index = 1;
    while index < args.len() {
        let flag = args[index].as_str();
        match flag {
            "--help" | "-h" => return Ok(None),
            "--manifest-path" => once(&mut manifest, PathBuf::from(value(&args, &mut index, flag)?), flag)?,
            "--cargo" => once(&mut cargo, PathBuf::from(value(&args, &mut index, flag)?), flag)?,
            "--package" => once(&mut package, value(&args, &mut index, flag)?.to_owned(), flag)?,
            "--scene" => once(&mut scene, value(&args, &mut index, flag)?.to_owned(), flag)?,
            "--bin" | "--example" => {
                let name = value(&args, &mut index, flag)?.to_owned();
                let item = if flag == "--bin" { CargoTarget::Bin(name) } else { CargoTarget::Example(name) };
                once(&mut target, item, "--bin/--example")?;
            }
            "--worker-arg" => worker_args.push(value(&args, &mut index, flag)?.to_owned()),
            "--feature" => features.push(value(&args, &mut index, flag)?.to_owned()),
            "--release" if !release => release = true,
            "--allow-network" if !allow_network => allow_network = true,
            "--build-timeout" => {
                let seconds = value(&args, &mut index, flag)?.parse::<u64>()?;
                if seconds == 0 || seconds > 86_400 {
                    return Err("--build-timeout must be between 1 and 86400 seconds".into());
                }
                once(&mut timeout, Duration::from_secs(seconds), flag)?;
            }
            _ => return Err(format!("unknown or repeated native-project option: {flag}").into()),
        }
        index += 1;
    }
    if worker_args.len() > 128 || features.len() > 128 {
        return Err("native-project worker arguments or features exceed 128 entries".into());
    }
    let cargo: PathBuf = cargo.ok_or("--cargo is required")?;
    if !cargo.is_absolute() {
        return Err("--cargo must select an absolute executable path".into());
    }
    let scene: String = scene.ok_or("--scene is required")?;
    if scene.len() > ProtocolLimits::default().max_field_bytes {
        return Err("scene name exceeds the Studio protocol budget".into());
    }
    Ok(Some(Options {
        manifest: manifest.ok_or("--manifest-path is required")?,
        cargo,
        package: package.ok_or("--package is required")?,
        target: target.ok_or("select exactly one --bin or --example")?,
        scene,
        worker_args,
        features,
        release,
        offline: !allow_network,
        timeout: timeout.unwrap_or(Duration::from_secs(300)),
    }))
}

pub(super) fn run_os(args: &[OsString]) -> fmn_cli::RunOutput {
    let options = match parse(args) {
        Ok(None) => return fmn_cli::RunOutput { code: 0, stdout: HELP.into(), stderr: String::new() },
        Ok(Some(options)) => options,
        Err(error) => return fmn_cli::RunOutput { code: 2, stdout: String::new(), stderr: format!("fmn studio-project: {error}\n\n{HELP}") },
    };
    let shutdown = Arc::new(AtomicBool::new(false));
    let input_shutdown = Arc::clone(&shutdown);
    let result = std::thread::Builder::new().name("fmn-project-stdin".into()).spawn(move || {
        let mut input = std::io::stdin().lock();
        let mut discard = [0_u8; 1024];
        while input.read(&mut discard).is_ok_and(|read| read != 0) {}
        input_shutdown.store(true, Ordering::Release);
    }).map_err(|error| Box::new(error) as Box<dyn Error>).and_then(|_| {
        run(options, &mut std::io::stdout().lock(), &shutdown)
    });
    shutdown.store(true, Ordering::Release);
    match result {
        Ok(()) => fmn_cli::RunOutput { code: 0, stdout: String::new(), stderr: String::new() },
        Err(error) => fmn_cli::RunOutput { code: 1, stdout: String::new(), stderr: format!("fmn studio-project: {error}\n") },
    }
}

fn compiler_environment(cargo: &Path) -> Result<Vec<(String, String)>> {
    let mut env = Vec::new();
    for key in ["PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN", "TMPDIR", "TMP", "TEMP", "SYSTEMROOT", "WINDIR", "USERPROFILE", "APPDATA", "LOCALAPPDATA"] {
        if let Some(value) = std::env::var_os(key) {
            env.push((key.into(), value.into_string().map_err(|_| format!("{key} is not UTF-8"))?));
        }
    }
    let rustc = cargo.parent().ok_or("Cargo has no directory")?.join(format!("rustc{}", std::env::consts::EXE_SUFFIX));
    if !rustc.is_file() {
        return Err("select Cargo beside its rustc (for example: rustup which cargo)".into());
    }
    env.push(("RUSTC".into(), rustc.to_str().ok_or("rustc path is not UTF-8")?.into()));
    Ok(env)
}

fn response(reply: SupervisorReply) -> Result<WorkerResponse> {
    match reply {
        SupervisorReply::Worker(WorkerResponse::Error { message, .. }) => Err(message.into()),
        SupervisorReply::Worker(response) => Ok(response),
        SupervisorReply::Recovered { .. } => Err("worker recovered; retry the operation".into()),
    }
}

// Declared before the supervisor so its private images are removed only after
// every worker and session has been dropped, including failed initial startup.
#[derive(Default)]
struct PublishedImages(Option<PathBuf>);

impl Drop for PublishedImages {
    fn drop(&mut self) {
        if let Some(directory) = self.0.take() {
            if let Err(error) = std::fs::remove_dir_all(&directory) {
                eprintln!("fmn studio-project: cannot remove retired worker images: {error}");
            }
        }
    }
}

// Stop the scoped watcher before its implicit join even when host setup or
// serving returns an error. Closing the hub also wakes idle preview readers.
struct StopWatch<'a> {
    stop: &'a AtomicBool,
    frames: &'a FrameHub,
}

impl Drop for StopWatch<'_> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.frames.close();
    }
}

fn run(options: Options, output: &mut dyn Write, shutdown: &AtomicBool) -> Result<()> {
    let path = if options.manifest.is_dir() { options.manifest.join("Cargo.toml") } else { options.manifest };
    let manifest = path.canonicalize()?;
    if manifest.file_name().is_none_or(|name| name != "Cargo.toml") || !manifest.is_file() {
        return Err("--manifest-path must select an existing Cargo.toml or project directory".into());
    }
    let project = manifest.parent().ok_or("manifest has no parent")?.to_path_buf();
    // Take the baseline BEFORE compiling: edits during the first build must
    // still be observed by the first live poll, not silently adopted as built.
    let mut watcher = SourceWatch::new_filtered(
        vec![project.join("Cargo.toml"), project.join("Cargo.lock"), project.join("src")],
        vec![project.join("target")],
        Duration::from_millis(300),
        WatchLimits::default(),
        WatchFilter {
            extensions: vec!["rs".into(), "toml".into()],
            ignored_directories: vec![".git".into()],
        },
    )?;
    let environment = compiler_environment(&options.cargo)?;
    let runner = Arc::new(StdProcessRunner);
    let triple = CargoRebuildDriver::host_triple(&*runner, &options.cargo, &environment)?;
    let mut config = CargoBuildConfig::new(options.cargo, manifest, options.package, options.target, triple,
        project.join("target/fmn-studio-build"), project.join("target/fmn-studio-workers"));
    config.build_env = environment;
    config.worker_argv = options.worker_args;
    config.worker_cwd = Some(project.clone());
    config.features = options.features;
    config.release = options.release;
    config.offline = options.offline;
    config.timeout = options.timeout;
    let mut builder = CargoRebuildDriver::new(config, runner)?;
    let clock: Arc<dyn Clock> = Arc::new(StdClock::new());
    let fs: Arc<dyn FileSystem> = Arc::new(VirtualFs::new());
    let cache = Store::open(fs, Arc::clone(&clock), project.join("target/fmn-project-cache"), StoreConfig::default())?
        .namespace("studio-replay", 1, NamespacePolicy { ceiling_bytes: Some(64 * 1024 * 1024) })?;
    let limits = ProtocolLimits::default();
    let mut images = PublishedImages::default();
    let mut supervisor = Supervisor::new(Box::new(StdWorkerLauncher::default()), Arc::clone(&clock), cache,
        SupervisorConfig { protocol_limits: limits, ..SupervisorConfig::default() });
    let scene = options.scene;
    supervisor.install_session(&scene, Journal::new())?;
    let started = supervisor.build_and_start(&mut builder);
    images.0 = builder.artifact_directory().map(Path::to_path_buf);
    started?;
    // Project and asset reads are not attested by this generic host. Rebuilds
    // conservatively replay the journal rather than reusing unknown checkpoints.
    response(supervisor.request(SupervisorRequest::Play { scene: scene.clone(), command: studio_seek_command(&scene, 0)? }, &|_| false)?)?;
    let WorkerResponse::Frame(initial) = response(supervisor.request(SupervisorRequest::Scrub { scene: scene.clone(), frame: 0 }, &|_| false)?)? else {
        return Err("selected worker did not return its native initial frame".into());
    };
    let session = Arc::new(StudioWorkerSession::new(&scene, supervisor, Box::new(builder), Arc::new(|_| false))?);
    let result = (|| -> Result<()> {
        let host_config = StudioHostConfig::default();
        let frames = FrameHub::new(host_config.max_frame_history, host_config.max_png_bytes)?;
        frames.publish(&initial, limits)?;
        let mut secret = [0_u8; 32];
        StdHostEntropy.fill(&mut secret)?;
        let token = CapabilityToken::new(secret)?;
        let cap = token.try_expose_hex()?;
        let host = StudioHost::bind(
            Arc::clone(&session), frames.clone(), token, Arc::clone(&clock), host_config,
        )?;
        let client = reload::Client::new(
            host.local_addr()?, cap, options.timeout.saturating_add(Duration::from_secs(30)),
        )?;
        let stop = AtomicBool::new(false);
        std::thread::scope(|scope| -> Result<()> {
            // Return (), not Box<dyn Error>: scoped thread results must be Send.
            let watcher_thread = std::thread::Builder::new()
                .name("fmn-project-source-watch".into())
                .spawn_scoped(scope, || {
                    let stopped = || stop.load(Ordering::Acquire) || shutdown.load(Ordering::Acquire);
                    let mut pending = reload::PendingReload::default();
                    let mut scan_error = String::new();
                    let mut rebuild_error = String::new();
                    while !stopped() {
                        match watcher.poll(clock.monotonic()) {
                            Ok(changed) => {
                                pending.observe(changed);
                                scan_error.clear();
                            }
                            Err(error) => {
                                let message = error.to_string();
                                if message != scan_error {
                                    eprintln!("fmn studio-project: source watch: {message}");
                                    scan_error = message;
                                }
                                std::thread::sleep(reload::POLL_INTERVAL);
                                continue;
                            }
                        }
                        if pending.ready(clock.monotonic()) && !stopped() {
                            let result = client.restart(&stopped);
                            pending.complete(clock.monotonic(), &result);
                            if stopped() { break; }
                            match result {
                                Ok(reload::Rebuild::Applied) => {
                                    eprintln!("fmn studio-project: source rebuilt; committed frame restored");
                                    rebuild_error.clear();
                                }
                                Ok(reload::Rebuild::Refused { status, detail }) => {
                                    eprintln!("fmn studio-project: rebuild refused ({status}); last published frame retained: {detail}");
                                    rebuild_error.clear();
                                }
                                Err(error) => {
                                    let message = error.to_string();
                                    if message != rebuild_error {
                                        eprintln!("fmn studio-project: rebuild transport: {message}; retrying");
                                        rebuild_error = message;
                                    }
                                }
                            }
                        }
                        std::thread::sleep(reload::POLL_INTERVAL);
                    }
                    // Wake multipart readers before serve_until joins them.
                    frames.close();
                })?;
            let guard = StopWatch { stop: &stop, frames: &frames };
            writeln!(output, "{}", host.launch_url()?)?;
            output.flush()?;
            let served = host.serve_until(shutdown);
            drop(guard);
            watcher_thread.join().map_err(|_| "source watcher panicked")?;
            served?;
            Ok(())
        })
    })();
    session.shutdown_worker();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> Vec<OsString> {
        let cargo = std::env::current_dir().unwrap().join("cargo");
        vec!["studio-project".into(), "--cargo".into(), cargo.into_os_string(), "--manifest-path".into(),
            "scene project/Cargo.toml".into(), "--package".into(), "demo".into(), "--bin".into(),
            "worker".into(), "--scene".into(), "MyScene".into()]
    }

    #[test]
    fn only_the_explicit_native_project_command_is_selected() {
        assert!(selected(&args()));
        for value in [vec![], vec!["studio".into()], vec!["render".into(), "studio-project".into()]] {
            assert!(!selected(&value));
        }
    }

    #[test]
    fn help_has_no_build_or_filesystem_precondition() {
        assert!(parse(&["studio-project".into(), "--help".into()]).unwrap().is_none());
        let output = run_os(&["studio-project".into(), "-h".into()]);
        assert_eq!(output.code, 0);
        assert!(output.stdout.contains("--manifest-path"));
        assert!(output.stderr.is_empty());
    }

    #[test]
    fn defaults_are_locked_offline_and_bounded() {
        let options = parse(&args()).unwrap().unwrap();
        assert!(options.offline);
        assert!(!options.release);
        assert_eq!(options.timeout, Duration::from_secs(300));
        assert_eq!(options.target, CargoTarget::Bin("worker".into()));
        assert_eq!(options.manifest, Path::new("scene project/Cargo.toml"));
    }

    #[test]
    fn worker_arguments_are_not_interpreted_as_compiler_flags() {
        let mut args = args();
        args.extend(["--worker-arg", "--worker", "--worker-arg", "--release", "--feature", "native", "--allow-network", "--build-timeout", "60"].map(OsString::from));
        let options = parse(&args).unwrap().unwrap();
        assert_eq!(options.worker_args, ["--worker", "--release"]);
        assert_eq!(options.features, ["native"]);
        assert!(!options.release);
        assert!(!options.offline);
        assert_eq!(options.timeout, Duration::from_secs(60));
    }

    #[test]
    fn examples_and_release_are_explicit_selections() {
        let mut args = args();
        args[7] = "--example".into();
        args.push("--release".into());
        let options = parse(&args).unwrap().unwrap();
        assert_eq!(options.target, CargoTarget::Example("worker".into()));
        assert!(options.release);
    }

    #[test]
    fn conflicting_missing_unknown_and_unbounded_requests_refuse() {
        for suffix in [vec!["--example", "other"], vec!["--scene", "Other"], vec!["--feature"],
            vec!["--build-timeout", "0"], vec!["--build-timeout", "86401"], vec!["--build-timeout", "NaN"],
            vec!["--worker-arg", ""], vec!["--worker-arg", "bad\0arg"], vec!["--unknown"]] {
            let mut args = args();
            args.extend(suffix.into_iter().map(OsString::from));
            assert!(parse(&args).is_err());
        }
        assert!(parse(&["studio-project".into()]).is_err());
        let mut args = args();
        args[2] = "cargo".into();
        assert!(parse(&args).is_err());
    }

    #[test]
    fn repeated_values_have_a_count_budget() {
        let mut args = args();
        for _ in 0..129 { args.extend(["--worker-arg".into(), "value".into()]); }
        assert!(parse(&args).is_err());
    }
}
