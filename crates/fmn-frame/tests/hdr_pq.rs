//! BT.2020/PQ output: standard reference vectors, highlight headroom, and
//! complete buffer/worker contracts. SDR/certified conversion stays separate.
#![forbid(unsafe_code)]

use fmn_frame::convert::{rgba16f_to_p010_hdr_pq, rgba16f_to_p010_hdr_pq_threaded};
use fmn_frame::half::f16_from_f32;
use fmn_frame::{
    ChromaSiting, ColorRange, FrameBuffer, FrameError, FrameLayout, HdrPqConfig, PixelFormat,
};

fn frame(format: PixelFormat, width: u32, height: u32, alignment: usize) -> FrameBuffer {
    let mut frame = FrameBuffer::new(
        FrameLayout::with_row_alignment(format, width, height, alignment).unwrap(),
    );
    frame.as_bytes_mut().fill(0xa5);
    frame
}

fn set_pixel(frame: &mut FrameBuffer, x: usize, y: usize, rgba: [f32; 4]) {
    let at = y * frame.layout().stride(0) + x * 8;
    for (channel, value) in rgba.into_iter().enumerate() {
        frame.plane_mut(0)[at + channel * 2..at + channel * 2 + 2]
            .copy_from_slice(&f16_from_f32(value).to_le_bytes());
    }
}

fn code(frame: &FrameBuffer, plane: usize, x: usize, y: usize) -> u16 {
    let at = y * frame.layout().stride(plane) + x * 2;
    let bytes = &frame.plane(plane)[at..at + 2];
    let sample = u16::from_le_bytes([bytes[0], bytes[1]]);
    assert_eq!(sample & 63, 0, "P010 low six bits must be zero");
    sample >> 6
}

fn uniform(rgb: [f32; 3], config: HdrPqConfig) -> [u16; 3] {
    let mut source = frame(PixelFormat::Rgba16F, 2, 2, 64);
    for y in 0..2 {
        for x in 0..2 {
            set_pixel(&mut source, x, y, [rgb[0], rgb[1], rgb[2], 1.0]);
        }
    }
    let mut output = frame(PixelFormat::P010, 2, 2, 64);
    rgba16f_to_p010_hdr_pq(
        &source,
        &mut output,
        ColorRange::Limited,
        ChromaSiting::Center,
        config,
    )
    .unwrap();
    [
        code(&output, 0, 0, 0),
        code(&output, 1, 0, 0),
        code(&output, 1, 1, 0),
    ]
}

fn padding_is_untouched(frame: &FrameBuffer) {
    let layout = frame.layout();
    for plane in 0..layout.format().plane_count() {
        let payload = layout
            .format()
            .min_row_bytes(layout.width(), plane)
            .unwrap();
        for row in frame.plane(plane).chunks_exact(layout.stride(plane)) {
            assert!(row[payload..].iter().all(|byte| *byte == 0xa5));
        }
    }
}

#[test]
fn config_validates_absolute_light_without_silent_clamping() {
    let defaults = HdrPqConfig::default();
    assert_eq!(defaults.reference_white_nits(), 203.0);
    assert_eq!(defaults.peak_nits(), 1_000.0);
    assert_eq!(defaults, HdrPqConfig::try_new(203.0, 1_000.0).unwrap());
    for white in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        -1.0,
        -0.0,
        0.0,
        10_001.0,
    ] {
        assert!(
            HdrPqConfig::try_new(white, 10_000.0).is_err(),
            "white={white}"
        );
    }
    for peak in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        -1.0,
        0.0,
        202.0,
        10_001.0,
    ] {
        assert!(HdrPqConfig::try_new(203.0, peak).is_err(), "peak={peak}");
    }
    for nits in [f64::MIN_POSITIVE, 1.0, 203.0, 10_000.0] {
        let config = HdrPqConfig::try_new(nits, nits).unwrap();
        assert_eq!(config.reference_white_nits(), nits);
        assert_eq!(config.peak_nits(), nits);
    }
}

#[test]
fn neutral_luminances_match_published_pq_transfer_and_legal_endpoints() {
    // BT.2100 table 4 inverse EOTF, followed by table 9's
    // round(64 + 876 * E′). These integer luminances are exactly representable
    // in binary16; the oracle is independent of the implementation's Q16 YUV.
    let config = HdrPqConfig::try_new(1.0, 10_000.0).unwrap();
    for (nits, expected) in [
        (0.0, 64),
        (1.0, 195),
        (10.0, 327),
        (100.0, 509),
        (203.0, 573),
        (1_000.0, 723),
        (4_000.0, 855),
        (10_000.0, 940),
    ] {
        assert_eq!(
            uniform([nits; 3], config),
            [expected, 512, 512],
            "{nits} nits"
        );
    }
}

#[test]
fn highlights_survive_reference_white_until_the_explicit_content_limit() {
    let config = HdrPqConfig::default();
    for (linear, expected) in [
        (1.0, 573),
        (2.0, 637),
        (4.0, 703),
        (8.0, 723),
        (65504.0, 723),
    ] {
        assert_eq!(uniform([linear; 3], config), [expected, 512, 512]);
    }
    let higher_limit = HdrPqConfig::try_new(203.0, 4_000.0).unwrap();
    assert!(uniform([8.0; 3], higher_limit)[0] > uniform([8.0; 3], config)[0]);
    assert_eq!(uniform([1.0; 3], higher_limit), uniform([1.0; 3], config));
}

#[test]
fn primary_conversion_precedes_pq_and_bt2020_nonconstant_luminance_matrix() {
    // Independent reference values: W3C's rational D65 sRGB -> XYZ -> BT.2020
    // matrices, then the BT.2100 table 4/6/9 formulae in real-valued arithmetic.
    // The implementation rounds transferred RGB to 16 bits and uses a Q16
    // matrix, so allow at most one final 10-bit code versus that reference.
    for (rgb, expected) in [
        ([1.0, 0.0, 0.0], [392, 438, 608]),
        ([0.0, 1.0, 0.0], [529, 424, 474]),
        ([0.0, 0.0, 1.0], [277, 667, 540]),
        ([2.0, 1.0, 0.5], [585, 478, 534]),
        ([0.0, 0.00390625, 0.125], [211, 608, 511]),
    ] {
        let actual = uniform(rgb, HdrPqConfig::default());
        for (got, want) in actual.into_iter().zip(expected) {
            assert!(
                got.abs_diff(want) <= 1,
                "rgb={rgb:?}: {actual:?} vs {expected:?}"
            );
        }
    }
}

#[test]
fn chroma_siting_averages_encoded_color_differences_after_pq() {
    let mut source = frame(PixelFormat::Rgba16F, 2, 2, 64);
    for (index, rgb) in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0; 3]]
        .into_iter()
        .enumerate()
    {
        set_pixel(
            &mut source,
            index % 2,
            index / 2,
            [rgb[0], rgb[1], rgb[2], 1.0],
        );
    }
    for (siting, chroma) in [
        (ChromaSiting::Left, [553, 574]),
        (ChromaSiting::Center, [510, 533]),
    ] {
        let mut output = frame(PixelFormat::P010, 2, 2, 64);
        rgba16f_to_p010_hdr_pq(
            &source,
            &mut output,
            ColorRange::Limited,
            siting,
            HdrPqConfig::default(),
        )
        .unwrap();
        for (index, expected) in [392, 529, 277, 573].into_iter().enumerate() {
            assert!(code(&output, 0, index % 2, index / 2).abs_diff(expected) <= 1);
        }
        for (channel, expected) in chroma.into_iter().enumerate() {
            assert!(code(&output, 1, channel, 0).abs_diff(expected) <= 1);
        }
    }
}

#[test]
fn nonfinite_channels_are_defined_and_alpha_does_not_change_composited_color() {
    let config = HdrPqConfig::default();
    assert_eq!(
        uniform([f32::NAN, f32::NEG_INFINITY, -1.0], config),
        [64, 512, 512]
    );
    assert_eq!(uniform([f32::INFINITY, 0.25, 0.0], config), [723, 512, 512]);
    let mut source = frame(PixelFormat::Rgba16F, 2, 2, 64);
    for (index, alpha) in [0.0, 1.0, f32::NAN, f32::INFINITY].into_iter().enumerate() {
        set_pixel(&mut source, index % 2, index / 2, [2.0, 1.0, 0.5, alpha]);
    }
    let mut output = frame(PixelFormat::P010, 2, 2, 64);
    rgba16f_to_p010_hdr_pq(
        &source,
        &mut output,
        ColorRange::Limited,
        ChromaSiting::Center,
        config,
    )
    .unwrap();
    let expected = uniform([2.0, 1.0, 0.5], config);
    for y in 0..2 {
        for x in 0..2 {
            assert_eq!(code(&output, 0, x, y), expected[0]);
        }
    }
    assert_eq!(
        [code(&output, 1, 0, 0), code(&output, 1, 1, 0)],
        [expected[1], expected[2]]
    );
}

#[test]
fn scoped_workers_preserve_padded_frames_and_output_orientation_byte_for_byte() {
    let (width, height) = (14, 258);
    let mut source = frame(PixelFormat::Rgba16F, width, height, 128);
    for y in 0..height as usize {
        for x in 0..width as usize {
            set_pixel(
                &mut source,
                x,
                y,
                [
                    x as f32 / 7.0,
                    y as f32 / 64.0,
                    ((x + y) % 13) as f32 / 8.0,
                    1.0,
                ],
            );
        }
    }
    let original = source.as_bytes().to_vec();
    let layout = FrameLayout::with_strides(PixelFormat::P010, width, height, &[64, 96]).unwrap();
    for siting in [ChromaSiting::Left, ChromaSiting::Center] {
        let mut expected = FrameBuffer::new(layout.clone());
        expected.as_bytes_mut().fill(0xa5);
        rgba16f_to_p010_hdr_pq(
            &source,
            &mut expected,
            ColorRange::Limited,
            siting,
            HdrPqConfig::default(),
        )
        .unwrap();
        padding_is_untouched(&expected);
        assert!(code(&expected, 0, 0, height as usize - 1) > code(&expected, 0, 0, 0));
        for plane in 0..2 {
            let rows = if plane == 0 { height } else { height / 2 };
            let legal = if plane == 0 { 64..=940 } else { 64..=960 };
            for y in 0..rows as usize {
                for x in 0..width as usize {
                    assert!(legal.contains(&code(&expected, plane, x, y)));
                }
            }
        }
        for threads in [0, 1, 2, 4, 16, usize::MAX] {
            let mut output = FrameBuffer::new(layout.clone());
            output.as_bytes_mut().fill(0xa5);
            rgba16f_to_p010_hdr_pq_threaded(
                &source,
                &mut output,
                ColorRange::Limited,
                siting,
                HdrPqConfig::default(),
                threads,
            )
            .unwrap();
            assert_eq!(
                output.as_bytes(),
                expected.as_bytes(),
                "{threads} threads, {siting:?}"
            );
        }
    }
    padding_is_untouched(&source);
    assert_eq!(source.as_bytes(), original);
}

#[test]
fn unsupported_layouts_are_refused_before_any_destination_write() {
    fn refuse(
        source: &FrameBuffer,
        mut output: FrameBuffer,
        range: ColorRange,
        expected: FrameError,
    ) {
        let before = output.as_bytes().to_vec();
        assert_eq!(
            rgba16f_to_p010_hdr_pq_threaded(
                source,
                &mut output,
                range,
                ChromaSiting::Center,
                HdrPqConfig::default(),
                4
            ),
            Err(expected)
        );
        assert_eq!(output.as_bytes(), before);
    }
    let source = frame(PixelFormat::Rgba16F, 2, 2, 64);
    refuse(
        &source,
        frame(PixelFormat::P010, 2, 2, 64),
        ColorRange::Full,
        FrameError::UnsupportedConversion("HDR PQ P010 output is limited-range only"),
    );
    refuse(
        &source,
        frame(PixelFormat::Nv12, 2, 2, 64),
        ColorRange::Limited,
        FrameError::FormatMismatch {
            expected: "P010 destination",
            got: PixelFormat::Nv12,
        },
    );
    refuse(
        &source,
        frame(PixelFormat::P010, 4, 2, 64),
        ColorRange::Limited,
        FrameError::DimensionMismatch,
    );
    refuse(
        &frame(PixelFormat::Rgba8, 2, 2, 64),
        frame(PixelFormat::P010, 2, 2, 64),
        ColorRange::Limited,
        FrameError::FormatMismatch {
            expected: "Rgba16F source",
            got: PixelFormat::Rgba8,
        },
    );
}
