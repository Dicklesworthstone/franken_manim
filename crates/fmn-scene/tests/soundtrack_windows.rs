//! Exact sample windows over validated portable PCM, independent of a mixer.
use fmn_hash::SerialError;
use fmn_scene::timeline_bundle::{BundleReadError, BundleSoundtrack};

fn track() -> BundleSoundtrack {
    BundleSoundtrack::new(11, 2, vec![0.125, -0.25, 0.375, -0.5, 0.625, -0.75]).unwrap()
}

#[test]
fn windows_copy_original_channel_bits_and_only_pad_after_the_source() {
    let source = track();
    for (start, frames, expected) in [
        (0, 2, vec![0.125, -0.25, 0.375, -0.5]),
        (1, 2, vec![0.375, -0.5, 0.625, -0.75]),
        (2, 3, vec![0.625, -0.75, 0.0, 0.0, 0.0, 0.0]),
        (3, 2, vec![0.0; 4]),
        (u64::MAX - 2, 2, vec![0.0; 4]),
        (u64::MAX, 0, vec![]),
    ] {
        let window = source
            .sample_window(start, frames, expected.len() * 4)
            .unwrap();
        assert_eq!(window.sample_rate(), 11);
        assert_eq!(window.channels(), 2);
        assert_eq!(window.sample_frames(), frames);
        assert_eq!(
            window
                .samples()
                .iter()
                .map(|s| s.to_bits())
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|s| f32::to_bits(*s))
                .collect::<Vec<_>>()
        );
    }
    assert_eq!(source, track());
}

#[test]
fn adjacent_unequal_windows_concatenate_to_the_same_absolute_interval() {
    let samples = (0..22)
        .map(|i| (i as f32 - 11.0) / 16.0)
        .collect::<Vec<_>>();
    let source = BundleSoundtrack::new(11, 1, samples.clone()).unwrap();
    // Independently rounded picture boundaries at 3 fps on an 11 Hz sample grid.
    // Rounding each *length* instead would give 4 + 4 + 4, not 4 + 3 + 4.
    let boundaries = [0, 4, 7, 11, 15, 18, 22];
    let mut joined = Vec::new();
    for span in boundaries.windows(2) {
        joined.extend(
            source
                .sample_window(span[0], span[1] - span[0], 16)
                .unwrap()
                .into_samples(),
        );
    }
    assert_eq!(joined, samples);
    let tail = source.sample_window(20, 5, 20).unwrap();
    assert_eq!(&tail.samples()[..2], &samples[20..]);
    assert_eq!(&tail.samples()[2..], &[0.0; 3]);
}

#[test]
fn padding_preserves_tails_bits_and_owned_storage_when_no_growth_is_needed() {
    let source = track();
    let ptr = source.samples().as_ptr();
    let same = source.pad_to(1, 24).unwrap();
    assert_eq!(same.samples().as_ptr(), ptr);
    assert_eq!(same, track());
    let padded = same.pad_to(5, 40).unwrap();
    assert_eq!(&padded.samples()[..6], track().samples());
    assert_eq!(&padded.samples()[6..], &[0.0; 4]);
    assert_eq!(padded.sample_frames(), 5);
    let empty = BundleSoundtrack::new(48_000, 1, vec![]).unwrap();
    assert!(empty.clone().pad_to(0, 0).unwrap().samples().is_empty());
    assert_eq!(empty.pad_to(2, 8).unwrap().samples(), &[0.0; 2]);
}

#[test]
fn bounds_refuse_before_allocation_and_include_existing_samples() {
    let source = track();
    assert!(matches!(
        source.sample_window(u64::MAX, 1, 8),
        Err(BundleReadError::Soundtrack(_))
    ));
    for result in [
        source.sample_window(0, 3, 23),
        source.clone().pad_to(0, 23),
        source.sample_window(0, u64::MAX, usize::MAX),
        source.clone().pad_to(u64::MAX, usize::MAX),
    ] {
        assert!(matches!(
            result,
            Err(BundleReadError::Malformed(SerialError::SizeLimit { .. }))
        ));
    }
    assert!(source.sample_window(2, 1, 8).is_ok());
    assert!(source.sample_window(2, 1, 7).is_err());
    assert!(source.sample_window(0, 0, 0).is_ok());
}
