//! Catalog identity is data; optimized cross-crate callers must not lose it.

use fmn_anim::{RateFunc, rate_from_tag, rate_tag};
use fmn_core::rate::{self, CatalogRate};

// Take these addresses in this integration-test crate, not in fmn-core's
// own unit tests. The original const catalog passed its self-comparison
// test while release consumers received different copies of the functions.
const CALLER_RATES: [fn(f64) -> f64; 8] = [
    rate::linear,
    rate::smooth,
    rate::rush_into,
    rate::rush_from,
    rate::slow_into,
    rate::double_smooth,
    rate::there_and_back,
    rate::lingering,
];

#[test]
fn decoded_rates_retain_identity_as_data_not_function_addresses() {
    for tag in 0..=u8::MAX {
        let decoded = rate_from_tag(tag);
        if tag >= 8 {
            assert!(decoded.is_none(), "unknown rate tag {tag}");
            assert!(CatalogRate::from_tag(tag).is_none());
            assert!(rate::from_tag(tag).is_none());
            continue;
        }
        let decoded = decoded.unwrap();
        assert!(matches!(&decoded, RateFunc::Catalog(rate) if rate.tag() == tag));
        assert_eq!(rate_tag(&decoded.clone()), Some(tag));
        assert_eq!(
            rate_tag(&RateFunc::Catalog(CatalogRate::from_tag(tag).unwrap())),
            Some(tag)
        );
    }
    assert!(matches!(
        RateFunc::linear(),
        RateFunc::Catalog(CatalogRate::Linear)
    ));
    assert!(matches!(
        RateFunc::default(),
        RateFunc::Catalog(CatalogRate::Smooth)
    ));
    assert!(matches!(
        RateFunc::smooth(),
        RateFunc::Catalog(CatalogRate::Smooth)
    ));
}

#[test]
fn nominal_catalog_functions_are_recognized_across_crate_boundaries() {
    for (index, function) in CALLER_RATES.into_iter().enumerate() {
        let tag = u8::try_from(index).unwrap();
        assert_eq!(rate::tag_of(function), Some(tag));
        assert_eq!(rate_tag(&RateFunc::Base(function)), Some(tag));
        assert_eq!(
            rate::tag_of(CatalogRate::from_tag(tag).unwrap().function()),
            Some(tag)
        );
    }
}

#[test]
fn catalog_representation_preserves_every_formula_bit_and_combinator() {
    for (index, function) in CALLER_RATES.into_iter().enumerate() {
        let tag = u8::try_from(index).unwrap();
        let decoded = rate_from_tag(tag).unwrap();
        let composed = decoded.clone().squish(0.2, 0.8).not_quite_there(0.7);
        let baseline = RateFunc::Base(function)
            .squish(0.2, 0.8)
            .not_quite_there(0.7);
        for sample in 0..=10_000 {
            let alpha = f64::from(sample) / 10_000.0;
            assert_eq!(decoded.eval(alpha).to_bits(), function(alpha).to_bits());
            assert_eq!(
                composed.eval(alpha).to_bits(),
                baseline.eval(alpha).to_bits()
            );
        }
        for alpha in [-2.0, -0.0, 0.0, 0.5, 1.0, 2.0] {
            assert_eq!(decoded.eval(alpha).to_bits(), function(alpha).to_bits());
        }
        assert_eq!(
            rate_tag(&composed),
            None,
            "a combinator is not its base tag"
        );
    }
}

#[test]
fn custom_and_sampled_curves_are_not_guessed_from_their_samples() {
    fn custom(alpha: f64) -> f64 {
        alpha * alpha
    }
    assert_eq!(rate_tag(&RateFunc::Base(custom)), None);
    assert_eq!(rate::tag_of(custom), None);
    assert_eq!(rate_tag(&RateFunc::Sampled(vec![0.0, 1.0].into())), None);
    assert_eq!(rate_tag(&RateFunc::linear().not_quite_there(1.0)), None);
    assert_eq!(rate_tag(&RateFunc::linear().squish(0.0, 1.0)), None);
}
