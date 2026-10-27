//! Byte-bound raster ingestion inside the existing supervised private session.

use super::*;
use crate::image::{
    DecodedImage, ImageDecodeError, ImageDecodeLimits, ImageInputFormat, decoded_image,
};

fn decode_argv(
    format: ImageInputFormat,
    input: &Path,
    output: &Path,
    max_pixels: u64,
    cap: u64,
) -> Vec<String> {
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
        format.demuxer(),
        "-max_pixels",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    argv.extend([
        max_pixels.to_string(),
        "-i".into(),
        input.display().to_string(),
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
            "png",
            "-pix_fmt",
            "rgba",
            "-f",
            "image2pipe",
            "-fs",
        ]
        .map(str::to_owned),
    );
    argv.extend([cap.to_string(), "-y".into(), output.display().to_string()]);
    argv
}

fn read_bounded_artifact(
    reader: impl Read,
    cap: u64,
) -> Result<Vec<u8>, BoundaryError> {
    let mut bytes = Vec::new();
    reader
        .take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| BoundaryError::Workdir {
            detail: format!("read bounded decoded image: {e}"),
        })?;
    if bytes.len() as u64 > cap {
        return Err(BoundaryError::ArtifactOversized {
            bytes: bytes.len() as u64,
            max: cap,
        });
    }
    Ok(bytes)
}

impl Boundary {
    /// Decode one immutable image byte snapshot to a native RGBA8 raster.
    /// The first frame is selected for animated inputs. No path is reopened,
    /// no format is probed, and no decoded artifact is publicly published.
    ///
    /// The process receives its pixel ceiling before opening the input; live
    /// file-growth monitoring, a bounded read and the owned PNG decoder then
    /// independently enforce the output and pixel budgets. External decoding
    /// is explicitly carried in the returned input receipt.
    pub fn decode_image_bytes(
        &self,
        bytes: &[u8],
        format: ImageInputFormat,
        limits: &ImageDecodeLimits,
    ) -> Result<DecodedImage, ImageDecodeError> {
        let cap = limits
            .decoded_byte_limit()?
            .min(self.limits.max_artifact_bytes);
        if cap == 0 {
            return Err(ImageDecodeError::InvalidLimits);
        }
        if bytes.len() as u64 > limits.max_input_bytes {
            return Err(ImageDecodeError::InputOversized {
                bytes: bytes.len() as u64,
                max: limits.max_input_bytes,
            });
        }
        if ImageInputFormat::recognize(bytes) != Some(format)
            || matches!(format, ImageInputFormat::Png | ImageInputFormat::Jpeg)
        {
            return Err(ImageDecodeError::UnsupportedInput);
        }
        let workdir = self.make_workdir()?;
        let result = (|| {
            let mut bound_tool = self.tool.bind_into(&workdir)?;
            let input = workdir.join("source.image");
            let artifact = workdir.join("decoded.png");
            let mut source = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&input)
                .map_err(|e| BoundaryError::Workdir {
                    detail: format!("create staged image: {e}"),
                })?;
            source
                .write_all(bytes)
                .and_then(|()| source.sync_all())
                .map_err(|e| BoundaryError::Workdir {
                    detail: format!("stage image snapshot: {e}"),
                })?;
            drop(source);
            let argv = decode_argv(format, &input, &artifact, limits.max_pixels, cap);
            workdir.verify_current("start image decode")?;
            bound_tool.verify_current(&self.tool)?;
            let spec = self.spec(bound_tool.path(), argv.clone(), &workdir, None);
            let outcome =
                self.run_artifact_process(&mut bound_tool, &spec, &workdir, &artifact, cap);
            workdir.verify_current("finish image decode")?;
            bound_tool.verify_current(&self.tool)?;
            let outcome = outcome?;
            self.check_outcome(&outcome)?;
            let (source_len, source_hash) = hash_private_artifact(&input, limits.max_input_bytes)?;
            if source_len != bytes.len() as u64 || source_hash != fmn_hash::sha256(bytes) {
                return Err(ImageDecodeError::InvalidRaster(
                    "staged source changed during decode",
                ));
            }
            verify_private_artifact(&artifact, cap)?;
            let file = File::open(&artifact).map_err(|e| BoundaryError::Workdir {
                detail: format!("read decoded image: {e}"),
            })?;
            let png = read_bounded_artifact(file, cap)?;
            let image = fmn_codec::decode_png(&png, &limits.png_limits())
                .map_err(ImageDecodeError::NativePng)?;
            let report = invocation_report(
                &self.tool,
                bound_tool.path(),
                self.runner.mechanism(),
                Some("png".into()),
                argv,
                artifact,
                outcome.stderr,
            );
            decoded_image(bytes, image.into(), format, Some(report))
        })();
        self.cleanup(&workdir);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_decode_is_one_bounded_first_frame_without_format_or_path_discovery() {
        for format in [
            ImageInputFormat::Gif,
            ImageInputFormat::Bmp,
            ImageInputFormat::WebP,
            ImageInputFormat::Tiff,
        ] {
            let argv = decode_argv(
                format,
                Path::new("/private/source.image"),
                Path::new("/private/decoded.png"),
                4096,
                9000,
            );
            for pair in [
                ["-protocol_whitelist", "file"],
                ["-f", format.demuxer()],
                ["-map", "0:v:0"],
                ["-max_pixels", "4096"],
                ["-fs", "9000"],
                ["-frames:v", "1"],
                ["-c:v", "png"],
                ["-pix_fmt", "rgba"],
            ] {
                assert!(argv.windows(2).any(|v| v == pair));
            }
            assert!(
                !argv
                    .iter()
                    .any(|v| ["-vf", "-s", "-filter_complex"].contains(&v.as_str()))
            );
        }
    }

    #[test]
    fn decoded_artifact_limit_is_inclusive() {
        use std::io::Cursor;

        let exact = vec![17_u8; 64];
        assert_eq!(
            read_bounded_artifact(Cursor::new(exact.clone()), 64).unwrap(),
            exact
        );

        let oversized = read_bounded_artifact(Cursor::new(vec![23_u8; 65]), 64).unwrap_err();
        assert!(matches!(
            oversized,
            BoundaryError::ArtifactOversized { bytes: 65, max: 64 }
        ));
    }
}
