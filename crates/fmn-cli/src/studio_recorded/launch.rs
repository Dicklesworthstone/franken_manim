//! Exact, versioned arguments for the same-executable recorded-picture worker.
//! The worker reads no configuration files or environment-selected scene code.

use super::*;
use fmn_core::color::LinearRgba;
use fmn_render::{
    Camera, CameraConfig, CameraFrame, EngineIdentity, FrameConfig, RetainedFrameRendererConfig,
    ScreenMap, Tiling, Viewport,
};

#[derive(Clone, Debug)]
pub(super) struct Launch {
    pub source_path: PathBuf,
    pub source_digest: String,
    pub scene: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub frame_height: f64,
    pub background: LinearRgba,
    pub threads: usize,
    pub seed: u64,
    pub config_digest: String,
}

impl Launch {
    fn fields(&self) -> Vec<String> {
        vec![
            self.source_digest.clone(),
            self.scene.clone(),
            self.width.to_string(),
            self.height.to_string(),
            self.fps.to_string(),
            format!("{:016x}", self.frame_height.to_bits()),
            format!("{:016x}", self.background.r.to_bits()),
            format!("{:016x}", self.background.g.to_bits()),
            format!("{:016x}", self.background.b.to_bits()),
            format!("{:016x}", self.background.a.to_bits()),
            self.threads.to_string(),
            self.seed.to_string(),
            self.config_digest.clone(),
        ]
    }

    pub fn argv(&self) -> Result<Vec<String>, Failure> {
        self.validate()?;
        let path = self
            .source_path
            .to_str()
            .ok_or_else(|| Failure::new("usage", "recorded worker source path must be UTF-8"))?;
        let mut args = vec![WORKER_ARG.to_owned(), path.to_owned()];
        args.extend(self.fields());
        Ok(args)
    }

    pub fn parse(args: &[OsString]) -> Result<Self, Failure> {
        if args.len() != 14 {
            return Err(Failure::new(
                "usage",
                "invalid recorded worker argument count",
            ));
        }
        let args = args
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .ok_or_else(|| Failure::new("usage", "recorded worker arguments must be UTF-8"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let number = |index: usize| {
            args[index]
                .parse::<u64>()
                .map_err(|error| Failure::new("usage", error))
        };
        let dimension =
            |index| u32::try_from(number(index)?).map_err(|error| Failure::new("usage", error));
        let float = |index: usize| {
            if args[index].len() != 16 {
                return Err(Failure::new(
                    "usage",
                    "invalid recorded worker binary64 field",
                ));
            }
            u64::from_str_radix(args[index], 16)
                .map(f64::from_bits)
                .map_err(|error| Failure::new("usage", error))
        };
        let value = Self {
            source_path: PathBuf::from(args[0]),
            source_digest: args[1].to_owned(),
            scene: args[2].to_owned(),
            width: dimension(3)?,
            height: dimension(4)?,
            fps: dimension(5)?,
            frame_height: float(6)?,
            background: LinearRgba {
                r: float(7)?,
                g: float(8)?,
                b: float(9)?,
                a: float(10)?,
            },
            threads: usize::try_from(number(11)?).map_err(|error| Failure::new("usage", error))?,
            seed: number(12)?,
            config_digest: args[13].to_owned(),
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), Failure> {
        let digest = |text: &str| {
            text.len() == 64
                && text
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        if !self.source_path.is_absolute()
            || self.source_path.as_os_str().len() > 4096
            || !digest(&self.source_digest)
            || !digest(&self.config_digest)
            || self.scene.is_empty()
            || self.scene.len() > 4096
            || self.scene.chars().any(char::is_control)
            || self.width == 0
            || self.height == 0
            || u64::from(self.width) * u64::from(self.height) > 16_777_216
            || self.fps == 0
            || !self.frame_height.is_finite()
            || self.frame_height <= 0.0
            || !(1..=4096).contains(&self.threads)
            || [
                self.background.r,
                self.background.g,
                self.background.b,
                self.background.a,
            ]
            .iter()
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
        {
            return Err(Failure::new(
                "config",
                "invalid or over-budget recorded Studio launch settings",
            ));
        }
        Ok(())
    }

    pub fn build_id(&self) -> Digest {
        // This is an ephemeral handshake identity, not a new durable schema.
        // Length-delimited fields prevent ambiguous concatenation; source paths
        // are transport locations, not part of the immutable recording identity.
        let mut bytes = b"fmn.cli.recorded-studio-worker/v1\0".to_vec();
        for field in std::iter::once(env!("FMN_BUILD_ID").to_owned()).chain(self.fields()) {
            bytes.extend_from_slice(&(field.len() as u64).to_le_bytes());
            bytes.extend_from_slice(field.as_bytes());
        }
        sha256(&bytes)
    }

    pub fn read(
        &self,
        fs: &dyn FileSystem,
    ) -> Result<(TimelineBundle, StudioRecordedAudio), Failure> {
        self.validate()?;
        let bytes = fs
            .read_bounded(&self.source_path, DEFAULT_MAX_BUNDLE_BYTES)
            .map_err(|error| Failure::new("scene", error))?;
        if sha256(&bytes).to_hex() != self.source_digest {
            return Err(Failure::new(
                "scene",
                "the recording changed; launch a new Studio session rather than pairing new pictures with old narration",
            ));
        }
        let (bundle, audio) = StudioRecordedAudio::decode(SOURCE_ID, &bytes)
            .map_err(|error| Failure::new("scene", error))?;
        let audio = audio.ok_or_else(|| {
            Failure::new(
                "scene",
                "recorded Studio worker requires the admitted soundtrack",
            )
        })?;
        self.view(&bundle)?;
        Ok((bundle, audio))
    }

    pub fn view(&self, bundle: &TimelineBundle) -> Result<CameraBundleView, Failure> {
        self.validate()?;
        if bundle.fps() != self.fps || bundle.frame_count() == 0 {
            return Err(Failure::new(
                "config",
                "recorded worker clock differs from the admitted recording",
            ));
        }
        let mut frame = CameraFrame::default();
        frame
            .set_shape([
                self.frame_height * f64::from(self.width) / f64::from(self.height),
                self.frame_height,
            ])
            .map_err(|error| Failure::new("config", error))?;
        let camera = Camera::new(CameraConfig {
            resolution: (self.width, self.height),
            fps: self.fps,
            background: self.background,
            frame,
            ..CameraConfig::default()
        })
        .map_err(|error| Failure::new("config", error))?;
        Ok(CameraBundleView {
            renderer: RetainedFrameRendererConfig {
                frame: FrameConfig::new(
                    Viewport {
                        width: self.width,
                        height: self.height,
                    },
                    ScreenMap {
                        scale: 1.0 / camera.pixel_size(),
                        origin: [f64::from(self.width) * 0.5, f64::from(self.height) * 0.5],
                        y_up: true,
                    },
                    self.background,
                ),
                tiling: Tiling::default(),
                engine: EngineIdentity::certified(),
                threads: self.threads,
            },
            seed: self.seed,
            fixed_camera: (!bundle.has_camera_track()).then_some(camera),
        })
    }
}
