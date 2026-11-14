//! Independent color algebra and storage contracts for native movie grading.

use std::collections::BTreeSet;

use fmn_frame::{FrameBuffer, FrameError, FrameLayout, PixelFormat, VideoColorGrade};

#[test]
fn settings_validate_before_preparation_and_retain_receipt_values() {
    for gamma in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        -1.0,
        0.0,
        0.09,
        10.01,
    ] {
        assert_eq!(
            VideoColorGrade::new(gamma, 1.0).unwrap_err(),
            FrameError::InvalidVideoGamma
        );
    }
    for saturation in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.01, 3.01] {
        assert_eq!(
            VideoColorGrade::new(1.0, saturation).unwrap_err(),
            FrameError::InvalidVideoSaturation
        );
    }
    for (gamma, saturation) in [(0.1, 0.0), (10.0, 3.0), (2.0, 1.5), (1.0, 1.0)] {
        let grade = VideoColorGrade::new(gamma, saturation).unwrap();
        assert_eq!(grade.gamma(), gamma);
        assert_eq!(grade.saturation(), saturation);
        assert_eq!(grade.is_identity(), gamma == 1.0 && saturation == 1.0);
    }
    let nearest_nonidentity =
        VideoColorGrade::new(f64::from_bits(1.0_f64.to_bits() + 1), 1.0).unwrap();
    assert!(
        !nearest_nonidentity.is_identity(),
        "no epsilon discards a requested grade"
    );
    let default = VideoColorGrade::default();
    assert!(default.is_identity());
    assert_eq!((default.gamma(), default.saturation()), (1.0, 1.0));
    assert!(
        FrameError::InvalidVideoGamma
            .to_string()
            .contains("0.1..=10")
    );
    assert!(
        FrameError::InvalidVideoSaturation
            .to_string()
            .contains("0..=3")
    );
}

#[test]
fn identity_preserves_all_bytes_and_buffer_ownership_in_every_output_format() {
    for format in [
        PixelFormat::Rgba8,
        PixelFormat::Bgra8,
        PixelFormat::Nv12,
        PixelFormat::P010,
    ] {
        let mut frame =
            FrameBuffer::new(FrameLayout::with_row_alignment(format, 6, 4, 64).unwrap());
        // Include out-of-range YUV codes, every possible alpha and arbitrary
        // P010 low bits: the identity must not canonicalize any of them.
        for (index, byte) in frame.as_bytes_mut().iter_mut().enumerate() {
            *byte = index.wrapping_mul(73).wrapping_add(19) as u8;
        }
        let before = frame.as_bytes().to_vec();
        let allocation = frame.as_bytes().as_ptr();
        VideoColorGrade::default().apply(&mut frame).unwrap();
        VideoColorGrade::new(1.0, 1.0)
            .unwrap()
            .apply(&mut frame)
            .unwrap();
        assert_eq!(frame.as_bytes(), before, "{format:?}");
        assert_eq!(frame.as_bytes().as_ptr(), allocation);
    }
}

fn quantize(value: f64) -> u8 {
    (255.0 * value.clamp(0.0, 1.0) + 0.5).floor() as u8
}

/// Explicitly travel through Y′CbCr and back, rather than using the kernel's
/// algebraically reduced per-channel expression. Gamma fixtures use square
/// and square root, independent of the production deterministic pow routine.
fn rgb_oracle(pixel: [u8; 4], gamma: f64, saturation: f64) -> [u8; 4] {
    let [r, g, b] = [pixel[0], pixel[1], pixel[2]].map(|value| f64::from(value) / 255.0);
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let cb = saturation * (b - y) / (2.0 * (1.0 - 0.0722));
    let cr = saturation * (r - y) / (2.0 * (1.0 - 0.2126));
    let y = match gamma {
        1.0 => y,
        2.0 => y.sqrt(),
        0.5 => y * y,
        _ => panic!("oracle has no independent formula for gamma {gamma}"),
    };
    let r = y + 2.0 * (1.0 - 0.2126) * cr;
    let b = y + 2.0 * (1.0 - 0.0722) * cb;
    let g = (y - 0.2126 * r - 0.0722 * b) / 0.7152;
    [quantize(r), quantize(g), quantize(b), pixel[3]]
}

#[test]
fn rgb_and_bgra_match_independent_color_algebra_preserve_alpha_and_padding() {
    let colors = [
        [255, 0, 0, 0],
        [0, 255, 0, 64],
        [0, 0, 255, 127],
        [128, 128, 128, 255],
        [12, 140, 220, 1],
        [240, 160, 20, 128],
        [0, 0, 0, 17],
        [255, 255, 255, 254],
    ];
    for format in [PixelFormat::Rgba8, PixelFormat::Bgra8] {
        for (gamma, saturation) in [
            (1.0, 0.0),
            (1.0, 0.5),
            (1.0, 1.5),
            (1.0, 3.0),
            (2.0, 1.0),
            (0.5, 1.0),
            (2.0, 0.5),
        ] {
            let mut frame =
                FrameBuffer::new(FrameLayout::with_strides(format, 4, 2, &[32]).unwrap());
            frame.as_bytes_mut().fill(0xA7);
            for (index, color) in colors.iter().enumerate() {
                let mut color = *color;
                if format == PixelFormat::Bgra8 {
                    color.swap(0, 2);
                }
                let offset = index / 4 * 32 + index % 4 * 4;
                frame.plane_mut(0)[offset..offset + 4].copy_from_slice(&color);
            }
            let allocation = frame.as_bytes().as_ptr();
            VideoColorGrade::new(gamma, saturation)
                .unwrap()
                .apply(&mut frame)
                .unwrap();
            assert_eq!(frame.as_bytes().as_ptr(), allocation);
            for (index, color) in colors.iter().enumerate() {
                let mut expected = rgb_oracle(*color, gamma, saturation);
                if format == PixelFormat::Bgra8 {
                    expected.swap(0, 2);
                }
                let offset = index / 4 * 32 + index % 4 * 4;
                assert_eq!(
                    &frame.plane(0)[offset..offset + 4],
                    &expected,
                    "{format:?}, {color:?}, gamma={gamma}, saturation={saturation}"
                );
            }
            for row in frame.plane(0).as_chunks::<32>().0 {
                assert!(row[16..].iter().all(|byte| *byte == 0xA7));
            }
        }
    }
}

#[test]
fn grayscale_uses_bt709_luma_and_gamma_keeps_black_and_white_fixed() {
    let colors = [
        [255, 0, 0, 10],
        [0, 255, 0, 20],
        [0, 0, 255, 30],
        [128, 128, 128, 40],
    ];
    let mut frame = FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 4, 1).unwrap());
    for (pixel, color) in frame
        .plane_mut(0)
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(colors)
    {
        *pixel = color;
    }
    VideoColorGrade::new(1.0, 0.0)
        .unwrap()
        .apply(&mut frame)
        .unwrap();
    assert_eq!(
        frame.as_bytes(),
        &[
            54, 54, 54, 10, 182, 182, 182, 20, 18, 18, 18, 30, 128, 128, 128, 40
        ]
    );

    for gamma in [0.1, 0.5, 2.0, 10.0] {
        let mut endpoints = FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 2, 1).unwrap());
        endpoints
            .as_bytes_mut()
            .copy_from_slice(&[0, 0, 0, 33, 255, 255, 255, 99]);
        VideoColorGrade::new(gamma, 3.0)
            .unwrap()
            .apply(&mut endpoints)
            .unwrap();
        assert_eq!(endpoints.as_bytes(), &[0, 0, 0, 33, 255, 255, 255, 99]);
    }
}

#[test]
fn nv12_all_codes_follow_legal_range_luma_and_centered_chroma_algebra() {
    let mut frame =
        FrameBuffer::new(FrameLayout::with_strides(PixelFormat::Nv12, 32, 16, &[48, 64]).unwrap());
    frame.as_bytes_mut().fill(0xB7);
    for plane in 0..2 {
        let stride = frame.layout().stride(plane);
        for (row_index, row) in frame.plane_mut(plane).chunks_exact_mut(stride).enumerate() {
            for (column, byte) in row[..32].iter_mut().enumerate() {
                *byte = (row_index * 32 + column) as u8;
            }
        }
    }
    let before = frame.clone();
    let allocation = frame.as_bytes().as_ptr();
    VideoColorGrade::new(2.0, 1.5)
        .unwrap()
        .apply(&mut frame)
        .unwrap();
    assert_eq!(frame.as_bytes().as_ptr(), allocation);
    for plane in 0..2 {
        let stride = frame.layout().stride(plane);
        for (source, actual) in before
            .plane(plane)
            .chunks_exact(stride)
            .zip(frame.plane(plane).chunks_exact(stride))
        {
            for (&source, &actual) in source[..32].iter().zip(&actual[..32]) {
                let expected = if plane == 0 {
                    let luma = f64::from(source.clamp(16, 235) - 16) / 219.0;
                    (16.0 + 219.0 * luma.sqrt() + 0.5).floor() as u8
                } else {
                    (128.0 + 1.5 * (f64::from(source.clamp(16, 240)) - 128.0))
                        .clamp(16.0, 240.0)
                        .round() as u8
                };
                assert_eq!(actual, expected, "plane={plane}, code={source}");
            }
            assert!(actual[32..].iter().all(|byte| *byte == 0xB7));
        }
    }
}

#[test]
fn p010_grades_all_ten_bits_without_an_eight_bit_intermediate() {
    let mut frame = FrameBuffer::new(
        FrameLayout::with_strides(PixelFormat::P010, 64, 32, &[144, 160]).unwrap(),
    );
    frame.as_bytes_mut().fill(0xC9);
    for plane in 0..2 {
        let stride = frame.layout().stride(plane);
        for (row_index, row) in frame.plane_mut(plane).chunks_exact_mut(stride).enumerate() {
            for (column, sample) in row[..128].as_chunks_mut::<2>().0.iter_mut().enumerate() {
                let code = ((row_index * 64 + column) % 1024) as u16;
                // Nonidentity output canonicalizes ignored low bits.
                *sample = ((code << 6) | 0x15).to_le_bytes();
            }
        }
    }
    let before = frame.clone();
    let allocation = frame.as_bytes().as_ptr();
    VideoColorGrade::new(2.0, 0.5)
        .unwrap()
        .apply(&mut frame)
        .unwrap();
    assert_eq!(frame.as_bytes().as_ptr(), allocation);
    let mut distinct_luma = BTreeSet::new();
    for plane in 0..2 {
        let stride = frame.layout().stride(plane);
        for (source, actual) in before
            .plane(plane)
            .chunks_exact(stride)
            .zip(frame.plane(plane).chunks_exact(stride))
        {
            for (source, actual) in source[..128]
                .as_chunks::<2>()
                .0
                .iter()
                .zip(actual[..128].as_chunks::<2>().0)
            {
                let source = u16::from_le_bytes(*source) >> 6;
                let packed = u16::from_le_bytes(*actual);
                assert_eq!(packed & 0x3f, 0);
                let actual = packed >> 6;
                let expected = if plane == 0 {
                    let luma = f64::from(source.clamp(64, 940) - 64) / 876.0;
                    (64.0 + 876.0 * luma.sqrt() + 0.5).floor() as u16
                } else {
                    (512.0 + 0.5 * (f64::from(source.clamp(64, 960)) - 512.0))
                        .clamp(64.0, 960.0)
                        .round() as u16
                };
                assert_eq!(actual, expected, "plane={plane}, code={source}");
                if plane == 0 {
                    distinct_luma.insert(actual);
                }
            }
            assert!(actual[128..].iter().all(|byte| *byte == 0xC9));
        }
    }
    assert!(
        distinct_luma.len() > 256,
        "grading must retain more than eight bits of luma"
    );
    assert!(distinct_luma.iter().any(|code| code % 4 != 0));
}

#[test]
fn linear_input_refuses_before_mutating_even_for_identity() {
    let mut frame =
        FrameBuffer::new(FrameLayout::with_row_alignment(PixelFormat::Rgba16F, 2, 2, 32).unwrap());
    frame.as_bytes_mut().fill(0xA3);
    let before = frame.as_bytes().to_vec();
    for grade in [
        VideoColorGrade::default(),
        VideoColorGrade::new(2.0, 0.5).unwrap(),
    ] {
        assert!(matches!(
            grade.apply(&mut frame),
            Err(FrameError::UnsupportedConversion(_))
        ));
        assert_eq!(frame.as_bytes(), before);
    }
}
