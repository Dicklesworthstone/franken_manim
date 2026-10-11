//! Exercise the production portal adapter, not just the shared frame kernels.

use std::collections::BTreeSet;

use fmn_frame::convert::{rgba_to_nv12, rgba_to_p010, rgba16f_to_rgba8, swap_rb8};
use fmn_frame::half::f16_from_f32;
use fmn_frame::transfer::srgb_decode;
use fmn_frame::{ChromaSiting, ColorRange, FrameBuffer, FrameLayout, PixelFormat};

use super::convert_frame;

fn frame(format: PixelFormat, width: u32, height: u32) -> FrameBuffer {
    let mut frame =
        FrameBuffer::new(FrameLayout::with_row_alignment(format, width, height, 64).unwrap());
    frame.as_bytes_mut().fill(0xA5);
    frame
}

fn set_pixel(frame: &mut FrameBuffer, x: usize, y: usize, channels: [u16; 4]) {
    let at = y * frame.layout().stride(0) + x * 8;
    for (i, bits) in channels.into_iter().enumerate() {
        frame.plane_mut(0)[at + i * 2..at + i * 2 + 2].copy_from_slice(&bits.to_le_bytes());
    }
}

fn codes(frame: &FrameBuffer, plane: usize) -> Vec<u16> {
    let layout = frame.layout();
    let payload = layout.format().min_row_bytes(layout.width(), plane).unwrap();
    frame
        .plane(plane)
        .chunks_exact(layout.stride(plane))
        .flat_map(|row| row[..payload].chunks_exact(2))
        .map(|bytes| {
            let word = u16::from_le_bytes([bytes[0], bytes[1]]);
            assert_eq!(word & 63, 0, "P010 samples are MSB-aligned");
            word >> 6
        })
        .collect()
}

#[test]
fn portal_p010_retains_more_than_eight_bits_of_gray() {
    let width = 4096usize;
    let mut source = frame(PixelFormat::Rgba16F, width as u32, 2);
    for x in 0..width {
        let gray = f16_from_f32(srgb_decode(x as f64 / (width - 1) as f64) as f32);
        for y in 0..2 {
            set_pixel(&mut source, x, y, [gray, gray, gray, 0x3c00]);
        }
    }
    let mut actual = frame(PixelFormat::P010, width as u32, 2);
    let mut legacy = frame(PixelFormat::P010, width as u32, 2);
    let mut scratch = frame(PixelFormat::Rgba8, width as u32, 2);
    let untouched = scratch.as_bytes().to_vec();
    convert_frame(&source, &mut actual, Some(&mut scratch)).unwrap();
    assert_eq!(scratch.as_bytes(), untouched);

    // This is the old production route, retained here as a planted negative:
    // widening its 8-bit samples cannot recover the discarded gray levels.
    rgba16f_to_rgba8(&source, &mut scratch).unwrap();
    rgba_to_p010(&scratch, &mut legacy, ColorRange::Limited, ChromaSiting::Left).unwrap();
    let actual_codes = codes(&actual, 0);
    let distinct = actual_codes.iter().copied().collect::<BTreeSet<_>>();
    let legacy_distinct = codes(&legacy, 0).into_iter().collect::<BTreeSet<_>>();
    assert!(legacy_distinct.len() <= 256);
    assert!(distinct.len() > 800, "only {} luma codes", distinct.len());
    assert_eq!(actual_codes[0], 64);
    assert_eq!(actual_codes[width - 1], 940);
    assert!(actual_codes[..width].windows(2).all(|pair| pair[0] <= pair[1]));
    assert!(codes(&actual, 1).iter().all(|code| *code == 512));
}

#[test]
fn portal_eight_bit_wires_preserve_legacy_bytes_for_all_half_patterns() {
    let mut source = frame(PixelFormat::Rgba16F, 256, 256);
    for bits in 0..=u16::MAX {
        set_pixel(
            &mut source,
            usize::from(bits) % 256,
            usize::from(bits) / 256,
            [bits, bits.rotate_left(5), !bits, bits.rotate_right(3)],
        );
    }
    let mut rgba = frame(PixelFormat::Rgba8, 256, 256);
    rgba16f_to_rgba8(&source, &mut rgba).unwrap();
    for format in [PixelFormat::Rgba8, PixelFormat::Bgra8, PixelFormat::Nv12] {
        let mut actual = frame(format, 256, 256);
        convert_frame(&source, &mut actual, None).unwrap();
        let mut expected = frame(format, 256, 256);
        match format {
            PixelFormat::Rgba8 => rgba16f_to_rgba8(&source, &mut expected).unwrap(),
            PixelFormat::Bgra8 => swap_rb8(&rgba, &mut expected).unwrap(),
            PixelFormat::Nv12 => {
                rgba_to_nv12(&rgba, &mut expected, ColorRange::Limited, ChromaSiting::Left).unwrap();
            }
            _ => unreachable!(),
        }
        assert_eq!(actual.as_bytes(), expected.as_bytes(), "{format:?}");
    }
}

#[test]
fn portal_wire_selection_does_not_depend_on_a_scratch_buffer() {
    let mut source = frame(PixelFormat::Rgba16F, 6, 4);
    for y in 0..4 {
        for x in 0..6 {
            set_pixel(
                &mut source,
                x,
                y,
                [
                    f16_from_f32(x as f32 / 6.0),
                    f16_from_f32(y as f32 / 4.0),
                    f16_from_f32(0.18),
                    f16_from_f32((x + y) as f32 / 10.0),
                ],
            );
        }
    }
    let before = source.as_bytes().to_vec();
    for format in [
        PixelFormat::Rgba8,
        PixelFormat::Bgra8,
        PixelFormat::Nv12,
        PixelFormat::P010,
    ] {
        let mut without = frame(format, 6, 4);
        let mut with = frame(format, 6, 4);
        // Deliberately not even a matching RGBA layout. It must stay unused.
        let mut scratch = frame(PixelFormat::P010, 2, 2);
        let untouched = scratch.as_bytes().to_vec();
        convert_frame(&source, &mut without, None).unwrap();
        convert_frame(&source, &mut with, Some(&mut scratch)).unwrap();
        assert_eq!(with.as_bytes(), without.as_bytes(), "{format:?}");
        assert_eq!(scratch.as_bytes(), untouched);
    }
    assert_eq!(source.as_bytes(), before);
}

#[test]
fn portal_conversion_refusals_leave_output_untouched() {
    for (source_format, output_format, output_width) in [
        (PixelFormat::Rgba8, PixelFormat::P010, 4),
        (PixelFormat::Rgba8, PixelFormat::Nv12, 4),
        (PixelFormat::Rgba16F, PixelFormat::Rgba16F, 4),
        (PixelFormat::Rgba16F, PixelFormat::Rgba8, 2),
        (PixelFormat::Rgba16F, PixelFormat::Bgra8, 2),
        (PixelFormat::Rgba16F, PixelFormat::Nv12, 2),
        (PixelFormat::Rgba16F, PixelFormat::P010, 2),
    ] {
        let source = frame(source_format, 4, 4);
        let mut output = frame(output_format, output_width, 4);
        let before = output.as_bytes().to_vec();
        assert!(convert_frame(&source, &mut output, None).is_err());
        assert_eq!(output.as_bytes(), before);
    }
}
