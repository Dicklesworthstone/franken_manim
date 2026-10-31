//! Bounded, single-capture MJPEG preview encoding over the governed boundary.
//!
//! Each capture is a separate supervised ffmpeg job. The process capability
//! exposes stdout only at completion, so this API makes no persistent-encoder
//! or preview-throughput claim. Input is a validated opaque RGBA8 snapshot;
//! output is exactly one JPEG from the mpjpeg muxer, never arbitrary MIME.

use super::*;

const BOUNDARY_TAG: &str = "fmn-preview";
const HEADER_BUDGET: usize = 1024;
/// Maximum decoded pixels admitted by the browser preview.
pub const MAX_PREVIEW_PIXELS: u64 = 33_554_432;

/// One validated JPEG and the exact invocation that produced it.
#[derive(Debug)]
pub struct MjpegPreview {
    /// JPEG bytes, without the ffmpeg multipart envelope.
    pub jpeg: Vec<u8>,
    /// Tool identity, argv, process policy and bounded stderr.
    pub invocation: InvocationReport,
}

/// A preview cannot be encoded or admitted safely.
#[derive(Debug)]
pub enum MjpegPreviewError {
    /// Raw geometry, transparency or a resource bound is unsupported.
    InvalidFrame(&'static str),
    /// The muxer did not return exactly one complete JPEG part.
    InvalidMultipart(&'static str),
    /// The existing supervised ffmpeg boundary refused the job.
    Boundary(BoundaryError),
    /// The owned JPEG decoder refused the encoded frame.
    Jpeg(fmn_codec::JpegError),
}

impl std::fmt::Display for MjpegPreviewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidFrame(reason) => write!(f, "MJPEG preview frame: {reason}"),
            Self::InvalidMultipart(reason) => write!(f, "MJPEG preview multipart: {reason}"),
            Self::Boundary(error) => error.fmt(f),
            Self::Jpeg(error) => write!(f, "MJPEG preview JPEG: {error}"),
        }
    }
}

impl std::error::Error for MjpegPreviewError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Boundary(error) => Some(error),
            Self::Jpeg(error) => Some(error),
            Self::InvalidFrame(_) | Self::InvalidMultipart(_) => None,
        }
    }
}

impl From<BoundaryError> for MjpegPreviewError {
    fn from(error: BoundaryError) -> Self {
        Self::Boundary(error)
    }
}

fn preview_argv(
    width: u32,
    height: u32,
    input: &Path,
    output: &Path,
    cap: u64,
) -> Result<Vec<String>, MjpegPreviewError> {
    let input = input.to_str().ok_or(MjpegPreviewError::InvalidFrame(
        "private input path is not UTF-8",
    ))?;
    let output = output.to_str().ok_or(MjpegPreviewError::InvalidFrame(
        "private output path is not UTF-8",
    ))?;
    let mut argv: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-xerror",
        "-filter_threads",
        "1",
        "-threads",
        "1",
        "-protocol_whitelist",
        "file",
        "-f",
        "rawvideo",
        "-pixel_format",
        "rgba",
        "-video_size",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    argv.extend([
        format!("{width}x{height}"),
        "-framerate".into(),
        "1".into(),
        "-i".into(),
        input.to_owned(),
    ]);
    argv.extend(
        [
            "-map",
            "0:v:0",
            "-an",
            "-sn",
            "-dn",
            "-map_metadata",
            "-1",
            "-map_chapters",
            "-1",
            "-frames:v",
            "1",
            "-threads",
            "1",
            "-c:v",
            "mjpeg",
            "-q:v",
            "3",
            "-pix_fmt",
            "yuvj444p",
            "-f",
            "mpjpeg",
            "-boundary_tag",
            BOUNDARY_TAG,
            "-fs",
        ]
        .map(str::to_owned),
    );
    argv.extend([cap.to_string(), "-y".into(), output.to_owned()]);
    Ok(argv)
}

fn jpeg_part(bytes: &[u8], max_jpeg_bytes: usize) -> Result<&[u8], MjpegPreviewError> {
    use MjpegPreviewError::InvalidMultipart as Invalid;
    let prefix = b"--fmn-preview\r\n";
    let rest = bytes
        .strip_prefix(prefix)
        .ok_or(Invalid("wrong boundary"))?;
    let header_end = rest
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .filter(|&end| end <= HEADER_BUDGET)
        .ok_or(Invalid("missing or oversized header"))?;
    let header =
        std::str::from_utf8(&rest[..header_end]).map_err(|_| Invalid("header is not UTF-8"))?;
    let (mut content_type, mut length) = (false, None);
    for line in header.split("\r\n") {
        let (name, value) = line.split_once(':').ok_or(Invalid("malformed header"))?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-type") && !content_type {
            if value != "image/jpeg" {
                return Err(Invalid("part is not image/jpeg"));
            }
            content_type = true;
        } else if name.eq_ignore_ascii_case("content-length") && length.is_none() {
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(Invalid("invalid content length"));
            }
            length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| Invalid("content length overflow"))?,
            );
        } else {
            return Err(Invalid("duplicate or unexpected header"));
        }
    }
    let length = length
        .filter(|&length| content_type && length >= 4 && length <= max_jpeg_bytes)
        .ok_or(Invalid("missing metadata or JPEG byte budget exceeded"))?;
    let start = header_end + 4;
    let end = start
        .checked_add(length)
        .ok_or(Invalid("payload length overflow"))?;
    let jpeg = rest.get(start..end).ok_or(Invalid("truncated JPEG"))?;
    let trailer = rest.get(end..).ok_or(Invalid("missing trailer"))?;
    // ffmpeg ends a one-frame mpjpeg file with the next open boundary.
    // A complete MIME closing boundary is accepted too; a second part is not.
    if trailer != b"\r\n--fmn-preview\r\n" && trailer != b"\r\n--fmn-preview--\r\n" {
        return Err(Invalid("extra part, truncated trailer or trailing bytes"));
    }
    if !jpeg.starts_with(&[0xff, 0xd8]) || !jpeg.ends_with(&[0xff, 0xd9]) {
        return Err(Invalid("JPEG markers are missing"));
    }
    Ok(jpeg)
}

impl Boundary {
    /// Encode one opaque, tightly packed RGBA8 capture to a validated JPEG.
    ///
    /// The job uses the existing private executable copy, source snapshot,
    /// live output/scratch ceiling, timeout and process-tree cleanup. Native
    /// dimensions and the complete one-part MIME envelope are verified before
    /// any bytes can enter the browser stream. No public file is published.
    /// JPEG cannot retain transparency: use PNG when any alpha is not 255.
    pub fn encode_mjpeg_preview(
        &self,
        width: u32,
        height: u32,
        rgba: &[u8],
        max_jpeg_bytes: usize,
    ) -> Result<MjpegPreview, MjpegPreviewError> {
        let pixels = u64::from(width) * u64::from(height);
        let expected = pixels
            .checked_mul(4)
            .and_then(|bytes| usize::try_from(bytes).ok());
        if width == 0
            || height == 0
            || width > u32::from(u16::MAX)
            || height > u32::from(u16::MAX)
            || pixels > MAX_PREVIEW_PIXELS
            || expected != Some(rgba.len())
            || max_jpeg_bytes < 4
        {
            return Err(MjpegPreviewError::InvalidFrame(
                "invalid dimensions, RGBA length or byte budget",
            ));
        }
        if rgba.as_chunks::<4>().0.iter().any(|pixel| pixel[3] != 255) {
            return Err(MjpegPreviewError::InvalidFrame(
                "JPEG cannot preserve transparency; use PNG",
            ));
        }
        let cap = u64::try_from(max_jpeg_bytes)
            .ok()
            .and_then(|bytes| bytes.checked_add(HEADER_BUDGET as u64))
            .ok_or(MjpegPreviewError::InvalidFrame(
                "encoded byte budget overflow",
            ))?
            .min(self.limits.max_artifact_bytes);
        if cap == 0 {
            return Err(MjpegPreviewError::InvalidFrame("zero artifact budget"));
        }
        let workdir = self.make_workdir()?;
        let result = (|| {
            let mut bound_tool = self.tool.bind_into(&workdir)?;
            let input = workdir.join("preview.rgba");
            let artifact = workdir.join("preview.mjpeg");
            let mut source = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&input)
                .map_err(|error| BoundaryError::Workdir {
                    detail: format!("create preview snapshot: {error}"),
                })?;
            source
                .write_all(rgba)
                .and_then(|()| source.sync_all())
                .map_err(|error| BoundaryError::Workdir {
                    detail: format!("stage preview snapshot: {error}"),
                })?;
            drop(source);
            let argv = preview_argv(width, height, &input, &artifact, cap)?;
            workdir.verify_current("start MJPEG preview")?;
            bound_tool.verify_current(&self.tool)?;
            let spec = self.spec(bound_tool.path(), argv.clone(), &workdir, None);
            let outcome =
                self.run_artifact_process(&mut bound_tool, &spec, &workdir, &artifact, cap);
            workdir.verify_current("finish MJPEG preview")?;
            bound_tool.verify_current(&self.tool)?;
            let outcome = outcome?;
            self.check_outcome(&outcome)?;
            let (source_bytes, source_digest) = hash_private_artifact(&input, rgba.len() as u64)?;
            if source_bytes != rgba.len() as u64 || source_digest != fmn_hash::sha256(rgba) {
                return Err(MjpegPreviewError::InvalidFrame(
                    "staged preview snapshot changed",
                ));
            }
            verify_private_artifact(&artifact, cap)?;
            let file = File::open(&artifact).map_err(|error| BoundaryError::Workdir {
                detail: format!("open MJPEG preview: {error}"),
            })?;
            let mut multipart = Vec::new();
            file.take(cap.saturating_add(1))
                .read_to_end(&mut multipart)
                .map_err(|error| BoundaryError::Workdir {
                    detail: format!("read bounded MJPEG preview: {error}"),
                })?;
            if multipart.len() as u64 > cap {
                return Err(BoundaryError::ArtifactOversized {
                    bytes: multipart.len() as u64,
                    max: cap,
                }
                .into());
            }
            let jpeg = jpeg_part(&multipart, max_jpeg_bytes)?;
            let decoded =
                fmn_codec::decode_jpeg(jpeg, &fmn_codec::JpegLimits { max_pixels: pixels })
                    .map_err(MjpegPreviewError::Jpeg)?;
            if decoded.width != width || decoded.height != height || decoded.orientation != 1 {
                return Err(MjpegPreviewError::InvalidFrame(
                    "encoded JPEG dimensions or orientation changed",
                ));
            }
            Ok(MjpegPreview {
                jpeg: jpeg.to_vec(),
                invocation: invocation_report(
                    &self.tool,
                    bound_tool.path(),
                    self.runner.mechanism(),
                    Some("mjpeg".into()),
                    argv,
                    artifact,
                    outcome.stderr,
                ),
            })
        })();
        self.cleanup(&workdir);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(jpeg: &[u8]) -> Vec<u8> {
        let mut bytes = format!(
            "--fmn-preview\r\nContent-type: image/jpeg\r\nContent-length: {}\r\n\r\n",
            jpeg.len()
        )
        .into_bytes();
        bytes.extend_from_slice(jpeg);
        bytes.extend_from_slice(b"\r\n--fmn-preview\r\n");
        bytes
    }

    #[test]
    fn accepts_one_exact_length_jpeg_and_refuses_extra_truncated_or_ambiguous_parts() {
        let jpeg = [0xff, 0xd8, 42, 0xff, 0xd9];
        let complete = part(&jpeg);
        assert_eq!(jpeg_part(&complete, jpeg.len()).unwrap(), jpeg);
        assert!(jpeg_part(&complete, jpeg.len() - 1).is_err());
        for end in 0..complete.len() {
            assert!(
                jpeg_part(&complete[..end], jpeg.len()).is_err(),
                "truncation at {end}"
            );
        }
        let mut extra = complete.clone();
        extra.extend_from_slice(&part(&jpeg));
        assert!(jpeg_part(&extra, jpeg.len()).is_err());
        for header in [
            "Content-type: image/png\r\nContent-length: +5",
            "Content-type: image/png\r\nContent-length: 5",
            "Content-type: image/jpeg\r\nContent-length: +5",
            "Content-type: image/jpeg\r\nContent-length: 5\r\nContent-length: 5",
            "Content-type: image/jpeg\r\nContent-length: 5\r\nX-Extra: value",
        ] {
            let mut malformed = format!("--fmn-preview\r\n{header}\r\n\r\n").into_bytes();
            malformed.extend_from_slice(&jpeg);
            malformed.extend_from_slice(b"\r\n--fmn-preview\r\n");
            assert!(jpeg_part(&malformed, jpeg.len()).is_err(), "{header}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn preview_argv_refuses_non_utf8_instead_of_changing_the_private_path() {
        use std::os::unix::ffi::OsStrExt as _;
        let bad = Path::new(std::ffi::OsStr::from_bytes(b"/private/\xff"));
        assert!(preview_argv(1, 1, bad, Path::new("/private/out"), 1024).is_err());
        assert!(preview_argv(1, 1, Path::new("/private/in"), bad, 1024).is_err());
    }

    #[test]
    fn preview_argv_pins_raw_input_and_one_bounded_mpjpeg_output() {
        let argv = preview_argv(
            64,
            48,
            Path::new("/private/preview.rgba"),
            Path::new("/private/preview.mjpeg"),
            4096,
        )
        .unwrap();
        for pair in [
            ["-protocol_whitelist", "file"],
            ["-f", "rawvideo"],
            ["-pixel_format", "rgba"],
            ["-video_size", "64x48"],
            ["-frames:v", "1"],
            ["-c:v", "mjpeg"],
            ["-f", "mpjpeg"],
            ["-boundary_tag", "fmn-preview"],
            ["-fs", "4096"],
        ] {
            assert!(argv.windows(2).any(|value| value == pair), "{pair:?}");
        }
        assert!(
            !argv
                .iter()
                .any(|value| ["-vf", "-filter_complex", "-s"].contains(&value.as_str()))
        );
    }
}
