//! Named portal rates must retain their identity before export or cloning.
use fmn_anim::{RateFunc, rate_tag};

#[test]
fn named_portal_rates_are_symbolic_not_function_address_guesses() {
    for (tag, name) in [
        "linear",
        "smooth",
        "rush_into",
        "rush_from",
        "slow_into",
        "double_smooth",
        "there_and_back",
        "lingering",
    ]
    .into_iter()
    .enumerate()
    {
        let rate = super::named_rate_func(name).unwrap();
        let tag = u8::try_from(tag).unwrap();
        assert!(matches!(&rate, RateFunc::Catalog(value) if value.tag() == tag));
        assert_eq!(rate_tag(&rate.clone()), Some(tag));
        let function = fmn_core::rate::from_tag(tag).unwrap();
        for sample in 0..=1024 {
            let alpha = f64::from(sample) / 1024.0;
            assert_eq!(rate.eval(alpha).to_bits(), function(alpha).to_bits());
        }
    }
}

#[test]
fn uncreate_default_keeps_exact_reversed_smooth_without_a_wire_tag() {
    let rate = super::named_rate_func("smooth_reversed").unwrap();
    assert_eq!(rate_tag(&rate), None);
    for sample in 0..=1024 {
        let alpha = f64::from(sample) / 1024.0;
        assert_eq!(
            rate.eval(alpha).to_bits(),
            fmn_core::rate::smooth(1.0 - alpha).to_bits()
        );
    }
}
