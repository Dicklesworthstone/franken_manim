//! fm-a25 acceptance: NV12/P010 golden vectors vs reference matrices,
//! format round-trips within documented precision, certified-conversion
//! determinism, and stride honoring in every kernel.

use fmn_frame::convert::{rgba_to_nv12, rgba_to_p010, rgba16f_to_rgba8, swap_rb8};
use fmn_frame::half::{f16_from_f32, f16_to_f64};
use fmn_frame::transfer::{TransferTables, quantize8, srgb_decode, srgb_encode};
use fmn_frame::{ChromaSiting, ColorRange, FrameBuffer, FrameError, FrameLayout, PixelFormat};

/// A w×h RGBA8 buffer filled with one color.
fn uniform_rgba8(w: u32, h: u32, rgba: [u8; 4]) -> FrameBuffer {
    let mut b = FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, w, h).unwrap());
    for px in b.plane_mut(0).as_chunks_mut::<4>().0 {
        *px = rgba;
    }
    b
}

/// Convert a uniform color and return its (Y, Cb, Cr) NV12 codes.
fn nv12_codes(rgba: [u8; 4], range: ColorRange) -> (u8, u8, u8) {
    let src = uniform_rgba8(2, 2, rgba);
    let mut dst = FrameBuffer::new(FrameLayout::tight(PixelFormat::Nv12, 2, 2).unwrap());
    rgba_to_nv12(&src, &mut dst, range, ChromaSiting::Center).unwrap();
    (dst.plane(0)[0], dst.plane(1)[0], dst.plane(1)[1])
}

#[test]
fn nv12_bt709_limited_golden_vectors() {
    // The canonical BT.709 limited-range anchors.
    assert_eq!(
        nv12_codes([255, 255, 255, 255], ColorRange::Limited),
        (235, 128, 128)
    );
    assert_eq!(
        nv12_codes([0, 0, 0, 255], ColorRange::Limited),
        (16, 128, 128)
    );
    assert_eq!(
        nv12_codes([255, 0, 0, 255], ColorRange::Limited),
        (63, 102, 240)
    );
    assert_eq!(
        nv12_codes([0, 255, 0, 255], ColorRange::Limited),
        (173, 42, 26)
    );
    assert_eq!(
        nv12_codes([0, 0, 255, 255], ColorRange::Limited),
        (32, 240, 118)
    );
    // Neutral gray hits the exact chroma midpoint (the zero-sum
    // coefficient rows make this structural, not approximate).
    assert_eq!(
        nv12_codes([128, 128, 128, 255], ColorRange::Limited),
        (126, 128, 128)
    );
}

#[test]
fn nv12_bt709_full_golden_vectors() {
    assert_eq!(
        nv12_codes([255, 255, 255, 255], ColorRange::Full),
        (255, 128, 128)
    );
    assert_eq!(nv12_codes([0, 0, 0, 255], ColorRange::Full), (0, 128, 128));
    // Full-range red: Cr saturates (127.5 + 128 rounds to 256, clamps).
    assert_eq!(
        nv12_codes([255, 0, 0, 255], ColorRange::Full),
        (54, 99, 255)
    );
    assert_eq!(
        nv12_codes([0, 0, 255, 255], ColorRange::Full),
        (18, 255, 116)
    );
}

#[test]
fn p010_limited_golden_vectors() {
    let src = uniform_rgba8(2, 2, [255, 255, 255, 255]);
    let mut dst = FrameBuffer::new(FrameLayout::tight(PixelFormat::P010, 2, 2).unwrap());
    rgba_to_p010(&src, &mut dst, ColorRange::Limited, ChromaSiting::Center).unwrap();
    let y = u16::from_le_bytes([dst.plane(0)[0], dst.plane(0)[1]]);
    let cb = u16::from_le_bytes([dst.plane(1)[0], dst.plane(1)[1]]);
    let cr = u16::from_le_bytes([dst.plane(1)[2], dst.plane(1)[3]]);
    // 10-bit limited white: Y′ = 940, achromatic = 512, MSB-aligned.
    assert_eq!(y, 940 << 6);
    assert_eq!(cb, 512 << 6);
    assert_eq!(cr, 512 << 6);
    // Low 6 bits are structurally zero.
    assert_eq!(y & 0x3f, 0);

    let src = uniform_rgba8(2, 2, [0, 0, 0, 255]);
    rgba_to_p010(&src, &mut dst, ColorRange::Limited, ChromaSiting::Center).unwrap();
    let y = u16::from_le_bytes([dst.plane(0)[0], dst.plane(0)[1]]);
    assert_eq!(y, 64 << 6);
}

#[test]
fn p010_full_range_is_a_typed_refusal() {
    let src = uniform_rgba8(2, 2, [0, 0, 0, 255]);
    let mut dst = FrameBuffer::new(FrameLayout::tight(PixelFormat::P010, 2, 2).unwrap());
    assert!(matches!(
        rgba_to_p010(&src, &mut dst, ColorRange::Full, ChromaSiting::Center),
        Err(FrameError::UnsupportedConversion(_))
    ));
}

#[test]
fn bgra_source_matches_rgba_source() {
    let rgba = uniform_rgba8(2, 2, [10, 200, 30, 255]);
    let mut bgra = FrameBuffer::new(FrameLayout::tight(PixelFormat::Bgra8, 2, 2).unwrap());
    for px in bgra.plane_mut(0).as_chunks_mut::<4>().0 {
        *px = [30, 200, 10, 255]; // same color, BGRA order
    }
    let layout = FrameLayout::tight(PixelFormat::Nv12, 2, 2).unwrap();
    let mut from_rgba = FrameBuffer::new(layout.clone());
    let mut from_bgra = FrameBuffer::new(layout);
    rgba_to_nv12(
        &rgba,
        &mut from_rgba,
        ColorRange::Limited,
        ChromaSiting::Center,
    )
    .unwrap();
    rgba_to_nv12(
        &bgra,
        &mut from_bgra,
        ColorRange::Limited,
        ChromaSiting::Center,
    )
    .unwrap();
    assert_eq!(from_rgba.as_bytes(), from_bgra.as_bytes());
}

#[test]
fn chroma_siting_semantics() {
    // Left column red, right column blue.
    let mut src = FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 2, 2).unwrap());
    for row in 0..2 {
        let stride = src.layout().stride(0);
        let r = &mut src.plane_mut(0)[row * stride..row * stride + 8];
        r[..4].copy_from_slice(&[255, 0, 0, 255]);
        r[4..].copy_from_slice(&[0, 0, 255, 255]);
    }
    let layout = FrameLayout::tight(PixelFormat::Nv12, 2, 2).unwrap();

    // Left siting: chroma comes from the left (red) column alone.
    let mut left = FrameBuffer::new(layout.clone());
    rgba_to_nv12(&src, &mut left, ColorRange::Limited, ChromaSiting::Left).unwrap();
    assert_eq!((left.plane(1)[0], left.plane(1)[1]), (102, 240));

    // Center siting: the 2×2 box average of red and blue chroma.
    let mut center = FrameBuffer::new(layout);
    rgba_to_nv12(&src, &mut center, ColorRange::Limited, ChromaSiting::Center).unwrap();
    assert_eq!((center.plane(1)[0], center.plane(1)[1]), (171, 179));

    // Luma is per-pixel and independent of siting.
    for b in [&left, &center] {
        assert_eq!(b.plane(0)[0], 63); // red
        assert_eq!(b.plane(0)[1], 32); // blue
    }
}

#[test]
fn kernels_honor_strides_and_leave_padding_untouched() {
    let src = uniform_rgba8(2, 2, [255, 0, 0, 255]);
    let padded = FrameLayout::with_row_alignment(PixelFormat::Nv12, 2, 2, 64).unwrap();
    let mut dst = FrameBuffer::new(padded);
    dst.as_bytes_mut().fill(0xAB);
    rgba_to_nv12(&src, &mut dst, ColorRange::Limited, ChromaSiting::Center).unwrap();

    for row in 0..2 {
        let r = &dst.plane(0)[row * 64..(row + 1) * 64];
        assert_eq!(&r[..2], &[63, 63]);
        assert!(r[2..].iter().all(|&x| x == 0xAB), "luma padding touched");
    }
    let c = dst.plane(1);
    assert_eq!(&c[..2], &[102, 240]);
    assert!(
        c[2..64].iter().all(|&x| x == 0xAB),
        "chroma padding touched"
    );

    // Same discipline on the certified kernel, via a padded source too.
    let mut fsrc =
        FrameBuffer::new(FrameLayout::with_row_alignment(PixelFormat::Rgba16F, 2, 2, 64).unwrap());
    let one = f16_from_f32(1.0).to_le_bytes();
    for row in 0..2 {
        for x in 0..2 {
            let at = row * 64 + x * 8;
            for ch in 0..4 {
                fsrc.plane_mut(0)[at + ch * 2..at + ch * 2 + 2].copy_from_slice(&one);
            }
        }
    }
    let mut fdst =
        FrameBuffer::new(FrameLayout::with_row_alignment(PixelFormat::Rgba8, 2, 2, 32).unwrap());
    fdst.as_bytes_mut().fill(0xAB);
    rgba16f_to_rgba8(&fsrc, &mut fdst).unwrap();
    for row in 0..2 {
        let r = &fdst.plane(0)[row * 32..(row + 1) * 32];
        assert_eq!(&r[..8], &[255; 8]);
        assert!(r[8..].iter().all(|&x| x == 0xAB), "rgba8 padding touched");
    }
}

/// Write one RGBA16F pixel from f32 channel values.
fn put_f16_px(b: &mut FrameBuffer, x: usize, y: usize, rgba: [f32; 4]) {
    let stride = b.layout().stride(0);
    let at = y * stride + x * 8;
    for (ch, v) in rgba.iter().enumerate() {
        let bits = f16_from_f32(*v);
        b.plane_mut(0)[at + ch * 2..at + ch * 2 + 2].copy_from_slice(&bits.to_le_bytes());
    }
}

#[test]
fn certified_conversion_anchors() {
    let mut src = FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba16F, 2, 1).unwrap());
    put_f16_px(&mut src, 0, 0, [1.0, 0.5, 0.0, 0.5]);
    put_f16_px(&mut src, 1, 0, [f32::NAN, -1.0, f32::INFINITY, 1.0]);
    let mut dst = FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 2, 1).unwrap());
    rgba16f_to_rgba8(&src, &mut dst).unwrap();

    // linear 0.5 → sRGB 187.52 → 188; alpha stays linear: 0.5 → 128.
    assert_eq!(&dst.plane(0)[..4], &[255, 188, 0, 128]);
    // NaN → 0, negative clamps to 0, +inf clamps to 255.
    assert_eq!(&dst.plane(0)[4..8], &[0, 0, 255, 255]);
}

#[test]
fn certified_conversion_is_deterministic_and_matches_the_scalar_path() {
    // A varied deterministic pattern over every channel.
    let mut src = FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba16F, 16, 16).unwrap());
    let mut state = 0x9e37_79b9_u32;
    for y in 0..16 {
        for x in 0..16 {
            let stride = src.layout().stride(0);
            for ch in 0..4 {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let bits = (state >> 16) as u16;
                let at = y * stride + x * 8 + ch * 2;
                src.plane_mut(0)[at..at + 2].copy_from_slice(&bits.to_le_bytes());
            }
        }
    }

    let layout = FrameLayout::tight(PixelFormat::Rgba8, 16, 16).unwrap();
    let mut once = FrameBuffer::new(layout.clone());
    let mut twice = FrameBuffer::new(layout);
    rgba16f_to_rgba8(&src, &mut once).unwrap();
    rgba16f_to_rgba8(&src, &mut twice).unwrap();
    assert_eq!(once.as_bytes(), twice.as_bytes());

    // The table path IS the scalar fmn-dmath path, entry for entry.
    let t1 = TransferTables::build();
    let t2 = TransferTables::build();
    for bits in (0..=u16::MAX).step_by(7) {
        assert_eq!(t1.srgb8_from_f16(bits), t2.srgb8_from_f16(bits));
        let v = f16_to_f64(bits);
        let v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
        assert_eq!(
            t1.srgb8_from_f16(bits),
            quantize8(srgb_encode(v)),
            "srgb table diverges from scalar at {bits:#06x}"
        );
        assert_eq!(t1.linear8_from_f16(bits), quantize8(v));
    }
}

#[test]
fn srgb_round_trip_precision() {
    // Documented precision: one 8-bit quantization step in the encoded
    // domain, plus binary16 representation error on the way in.
    for i in 0..=64 {
        let linear = f64::from(i) / 64.0;
        let bits = f16_from_f32(linear as f32);
        let mut src = FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba16F, 2, 2).unwrap());
        for y in 0..2 {
            for x in 0..2 {
                put_f16_px(&mut src, x, y, [linear as f32; 4]);
            }
        }
        let mut dst = FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 2, 2).unwrap());
        rgba16f_to_rgba8(&src, &mut dst).unwrap();
        let byte = dst.plane(0)[0];
        let expected = srgb_encode(f16_to_f64(bits)) * 255.0;
        assert!(
            (f64::from(byte) - expected).abs() <= 0.5 + 1e-9,
            "linear {linear}: byte {byte} vs encoded {expected}"
        );
        // And decoding lands back near the original linear value.
        let back = srgb_decode(f64::from(byte) / 255.0);
        assert!((back - linear).abs() < 0.01, "linear {linear} → {back}");
    }
}

#[test]
fn swizzle_round_trip_and_refusals() {
    let mut src = FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 3, 2).unwrap());
    for (i, byte) in src.plane_mut(0).iter_mut().enumerate() {
        *byte = (i * 7 % 251) as u8;
    }
    let mut bgra = FrameBuffer::new(FrameLayout::tight(PixelFormat::Bgra8, 3, 2).unwrap());
    let mut back = FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 3, 2).unwrap());
    swap_rb8(&src, &mut bgra).unwrap();
    swap_rb8(&bgra, &mut back).unwrap();
    assert_eq!(src.as_bytes(), back.as_bytes());
    assert_ne!(src.as_bytes(), bgra.as_bytes());

    // Same-format "swizzle" is refused — it would be a silent copy.
    let mut same = FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 3, 2).unwrap());
    assert!(matches!(
        swap_rb8(&src, &mut same),
        Err(FrameError::FormatMismatch { .. })
    ));
}

#[test]
fn kernel_input_refusals() {
    let rgba = uniform_rgba8(2, 2, [0, 0, 0, 255]);
    let mut nv12 = FrameBuffer::new(FrameLayout::tight(PixelFormat::Nv12, 2, 2).unwrap());
    let mut wrong_dims = FrameBuffer::new(FrameLayout::tight(PixelFormat::Nv12, 4, 4).unwrap());
    let mut rgba8_dst = FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 2, 2).unwrap());

    assert_eq!(
        rgba_to_nv12(
            &rgba,
            &mut wrong_dims,
            ColorRange::Limited,
            ChromaSiting::Center
        ),
        Err(FrameError::DimensionMismatch)
    );
    assert!(matches!(
        rgba16f_to_rgba8(&rgba, &mut rgba8_dst),
        Err(FrameError::FormatMismatch { .. })
    ));
    assert!(matches!(
        rgba_to_nv12(
            &nv12.clone(),
            &mut nv12,
            ColorRange::Limited,
            ChromaSiting::Center
        ),
        Err(FrameError::FormatMismatch { .. })
    ));
}
