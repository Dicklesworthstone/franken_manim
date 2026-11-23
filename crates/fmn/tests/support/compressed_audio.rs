//! Script only the optional decoder process; keep production admission,
//! executable binding, native PCM decode/mix, and publication unchanged.

use fmn::rendering::FfmpegCapability;
use fmn_platform::fs::VirtualFs;
use fmn_platform::process::{
    FfmpegExecutable, FfmpegLocator, FfmpegLocatorError, ProcessCancellation, ProcessError,
    ProcessMechanism, ProcessOutcome, ProcessRunner, ProcessSpec, ProcessStdinLimits,
    ProcessTermination, RunningProcess, StdFfmpegLocator,
};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

struct ObservedLocator {
    requests: Arc<Mutex<Vec<PathBuf>>>,
}

impl FfmpegLocator for ObservedLocator {
    fn locate_ffmpeg(&self, path: &Path) -> Result<FfmpegExecutable, FfmpegLocatorError> {
        self.requests.lock().unwrap().push(path.to_owned());
        StdFfmpegLocator::default().locate_ffmpeg(path)
    }
}

struct DecoderRunner {
    runs: Arc<Mutex<Vec<ProcessSpec>>>,
    sources: Arc<Mutex<Vec<Vec<u8>>>>,
    output: Vec<u8>,
    refuse: bool,
    changed_source: Option<Arc<VirtualFs>>,
}

impl ProcessRunner for DecoderRunner {
    fn mechanism(&self) -> ProcessMechanism {
        ProcessMechanism::Scripted
    }

    fn start(
        &self,
        spec: &ProcessSpec,
        cancellation: ProcessCancellation,
        stdin_limits: ProcessStdinLimits,
    ) -> Result<Box<dyn RunningProcess>, ProcessError> {
        assert!(spec.stdin.is_none());
        assert!(spec.cwd.is_none());
        assert!(spec.program.is_absolute());
        assert_eq!(stdin_limits.max_total_bytes, 0);
        self.runs.lock().unwrap().push(spec.clone());
        let mut outcome = ProcessOutcome {
            termination: ProcessTermination::Exited(Some(0)),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        if spec.argv == ["-version"] {
            outcome.stdout = b"ffmpeg version bundle-audio-fixture\n".to_vec();
        } else {
            assert!(
                spec.argv
                    .windows(2)
                    .any(|pair| pair == ["-c:a", "pcm_f32le"])
            );
            assert!(
                spec.argv
                    .windows(2)
                    .any(|pair| pair == ["-protocol_whitelist", "file"])
            );
            assert!(!spec.argv.iter().any(|arg| arg == "-ar" || arg == "-ac"));
            let input = spec.argv.windows(2).find(|pair| pair[0] == "-i").unwrap();
            let source = std::fs::read(&input[1]).unwrap();
            self.sources.lock().unwrap().push(source);
            if self.refuse {
                outcome.termination = ProcessTermination::Exited(Some(9));
                outcome.stderr = b"fixture decoder refused the source".to_vec();
            } else {
                std::fs::write(spec.argv.last().unwrap(), &self.output).unwrap();
                outcome.stderr = b"fixture decoded original source bytes".to_vec();
            }
            if let Some(fs) = &self.changed_source {
                // A later repeated cue must use the already-read snapshot,
                // rather than reopening the source after external work.
                fs.insert("/voice.asset", b"changed during first decode".to_vec());
            }
        }
        Ok(Box::new(DecoderProcess {
            outcome,
            cancellation,
        }))
    }
}

struct DecoderProcess {
    outcome: ProcessOutcome,
    cancellation: ProcessCancellation,
}

impl RunningProcess for DecoderProcess {
    fn write_stdin(&mut self, bytes: &[u8]) -> Result<(), ProcessError> {
        assert!(bytes.is_empty(), "audio decode uses its staged source file");
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<ProcessOutcome, ProcessError> {
        if self.cancellation.is_cancelled() {
            Ok(ProcessOutcome {
                termination: ProcessTermination::Cancelled,
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        } else {
            Ok(self.outcome)
        }
    }

    fn cancel(self: Box<Self>) -> Result<(), ProcessError> {
        self.cancellation.cancel();
        Ok(())
    }
}

pub struct Fixture {
    pub tool: PathBuf,
    pub capability: FfmpegCapability,
    pub locates: Arc<Mutex<Vec<PathBuf>>>,
    pub runs: Arc<Mutex<Vec<ProcessSpec>>>,
    pub sources: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Fixture {
    pub fn new(changed_source: Option<Arc<VirtualFs>>, output: Vec<u8>, refuse: bool) -> Self {
        static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = PathBuf::from("/tmp").canonicalize().unwrap().join(format!(
            "fmn-compressed-audio-fixture-{}-{stamp}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::SeqCst),
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let tool = root.join("ffmpeg");
        std::fs::write(&tool, native_image()).unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let locates = Arc::new(Mutex::new(Vec::new()));
        let runs = Arc::new(Mutex::new(Vec::new()));
        let sources = Arc::new(Mutex::new(Vec::new()));
        Self {
            tool,
            capability: FfmpegCapability {
                locator: Arc::new(ObservedLocator {
                    requests: Arc::clone(&locates),
                }),
                runner: Arc::new(DecoderRunner {
                    runs: Arc::clone(&runs),
                    sources: Arc::clone(&sources),
                    output,
                    refuse,
                    changed_source,
                }),
                workdir_root: root,
            },
            locates,
            runs,
            sources,
        }
    }
}

// The same minimal structural containers used by Reel's existing scripted
// boundary tests. Keeping them tiny avoids copying the entire test binary.
fn native_image() -> Vec<u8> {
    #[cfg(target_os = "linux")]
    {
        let mut bytes = vec![0_u8; 120];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4..7].copy_from_slice(&[2, 1, 1]);
        bytes[16..18].copy_from_slice(&3_u16.to_le_bytes());
        let machine = if cfg!(target_arch = "x86_64") {
            62_u16
        } else {
            183_u16
        };
        bytes[18..20].copy_from_slice(&machine.to_le_bytes());
        bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
        bytes[24..32].copy_from_slice(&0x1000_u64.to_le_bytes());
        bytes[32..40].copy_from_slice(&64_u64.to_le_bytes());
        bytes[52..54].copy_from_slice(&64_u16.to_le_bytes());
        bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&1_u16.to_le_bytes());
        let segment = &mut bytes[64..];
        segment[..4].copy_from_slice(&1_u32.to_le_bytes());
        segment[4..8].copy_from_slice(&5_u32.to_le_bytes());
        segment[16..24].copy_from_slice(&0x1000_u64.to_le_bytes());
        segment[32..40].copy_from_slice(&120_u64.to_le_bytes());
        segment[40..48].copy_from_slice(&120_u64.to_le_bytes());
        segment[48..56].copy_from_slice(&0x1000_u64.to_le_bytes());
        bytes
    }
    #[cfg(target_os = "macos")]
    {
        let mut bytes = vec![0_u8; 201];
        bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        let cpu = if cfg!(target_arch = "x86_64") {
            0x0100_0007_u32
        } else {
            0x0100_000c_u32
        };
        let subtype = if cfg!(target_arch = "x86_64") {
            3_u32
        } else {
            0_u32
        };
        bytes[4..8].copy_from_slice(&cpu.to_le_bytes());
        bytes[8..12].copy_from_slice(&subtype.to_le_bytes());
        bytes[12..16].copy_from_slice(&2_u32.to_le_bytes());
        bytes[16..20].copy_from_slice(&3_u32.to_le_bytes());
        bytes[20..24].copy_from_slice(&168_u32.to_le_bytes());
        let pagezero = &mut bytes[32..104];
        pagezero[..4].copy_from_slice(&0x19_u32.to_le_bytes());
        pagezero[4..8].copy_from_slice(&72_u32.to_le_bytes());
        pagezero[8..18].copy_from_slice(b"__PAGEZERO");
        pagezero[32..40].copy_from_slice(&0x1_0000_0000_u64.to_le_bytes());
        let text = &mut bytes[104..176];
        text[..4].copy_from_slice(&0x19_u32.to_le_bytes());
        text[4..8].copy_from_slice(&72_u32.to_le_bytes());
        text[8..14].copy_from_slice(b"__TEXT");
        text[24..32].copy_from_slice(&0x1_0000_0000_u64.to_le_bytes());
        text[32..40].copy_from_slice(&201_u64.to_le_bytes());
        text[48..56].copy_from_slice(&201_u64.to_le_bytes());
        text[56..60].copy_from_slice(&7_u32.to_le_bytes());
        text[60..64].copy_from_slice(&5_u32.to_le_bytes());
        let entry = &mut bytes[176..200];
        entry[..4].copy_from_slice(&0x8000_0028_u32.to_le_bytes());
        entry[4..8].copy_from_slice(&24_u32.to_le_bytes());
        entry[8..16].copy_from_slice(&200_u64.to_le_bytes());
        bytes[200] = 0xc3;
        bytes
    }
}
