//! Dimension-independent ODE sampling uses the real suite integrator.
use fmn_library::{IntegratorTune, OdeError, SamplingBudget, try_ode_solution_points};

fn tune() -> IntegratorTune {
    IntegratorTune {
        rtol: 1e-3,
        atol: 1e-6,
        ..IntegratorTune::default()
    }
}

#[test]
fn adaptive_decay_samples_the_requested_grid() {
    let solution = try_ode_solution_points(
        |state| Ok::<_, ()>(state.iter().map(|value| -value).collect()),
        &[1.0],
        1.0,
        0.01,
        tune(),
        SamplingBudget::DEFAULT,
    )
    .unwrap();
    assert_eq!(solution.states.len(), 100);
    assert_eq!(solution.times[0], 0.0);
    assert_eq!(solution.times[99], 0.99);
    assert_eq!(solution.states[0], [1.0]);
    assert!(solution.nfev < solution.states.len());
    for (time, state) in solution.times.iter().zip(&solution.states) {
        let exact = fmn_dmath::exp(-time);
        assert!((state[0] / exact - 1.0).abs() < 1e-3);
    }
}

#[test]
fn coupled_six_dimensional_oscillators_keep_every_component() {
    let initial = [1.0, 0.0, 2.0, 0.0, 3.0, 0.0];
    let solution = try_ode_solution_points(
        |state| {
            Ok::<_, ()>(
                state
                    .chunks_exact(2)
                    .flat_map(|pair| [pair[1], -pair[0]])
                    .collect(),
            )
        },
        &initial,
        1.0,
        0.02,
        tune(),
        SamplingBudget::DEFAULT,
    )
    .unwrap();
    for (time, state) in solution.times.iter().zip(&solution.states) {
        assert_eq!(state.len(), initial.len());
        for (pair, radius) in state.chunks_exact(2).zip([1.0, 2.0, 3.0]) {
            assert!((pair[0] - radius * fmn_dmath::cos(*time)).abs() < 2e-3);
            assert!((pair[1] + radius * fmn_dmath::sin(*time)).abs() < 2e-3);
        }
    }
}

#[test]
fn visual_quality_tolerances_resolve_mixed_rate_components() {
    let initial = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
    let rates = [0.1, 0.2, 0.4, 0.8, 1.0, 1.5];
    let solution = try_ode_solution_points(
        |state| {
            Ok::<_, ()>(
                state
                    .iter()
                    .zip(&rates)
                    .map(|(value, rate)| -rate * value)
                    .collect(),
            )
        },
        &initial,
        1.0,
        0.01,
        IntegratorTune::default(),
        SamplingBudget::DEFAULT,
    )
    .unwrap();
    assert_eq!(solution.states.len(), 100);
    assert!(solution.nfev < solution.states.len());
    for (time, state) in solution.times.iter().zip(&solution.states) {
        for ((value, initial), rate) in state.iter().zip(&initial).zip(&rates) {
            let exact = initial * fmn_dmath::exp(-rate * time);
            assert!((value / exact - 1.0).abs() < 2e-6);
        }
    }
}

#[test]
fn output_cadence_does_not_become_the_solver_step() {
    for dt in [0.3, 2.0] {
        let solution = try_ode_solution_points(
            |_| Ok::<_, ()>(vec![1.0]),
            &[2.0],
            1.0,
            dt,
            tune(),
            SamplingBudget::DEFAULT,
        )
        .unwrap();
        let expected = if dt < 1.0 { 4 } else { 1 };
        assert_eq!(solution.states.len(), expected);
        for (time, state) in solution.times.iter().zip(&solution.states) {
            assert!(*time < 1.0);
            assert!((state[0] - (2.0 + time)).abs() < 1e-12);
        }
    }
}

#[test]
fn first_callback_error_stops_evaluation_and_preserves_its_value() {
    for stop in [1, 2, 3, 7, 12] {
        let mut calls = 0;
        let original = std::rc::Rc::new("authored cancellation");
        let result = try_ode_solution_points(
            |state| {
                calls += 1;
                if calls == stop {
                    return Err(std::rc::Rc::clone(&original));
                }
                Ok(vec![-state[0]])
            },
            &[1.0],
            1.0,
            0.01,
            tune(),
            SamplingBudget::DEFAULT,
        );
        let Err(OdeError::Callback(error)) = result else {
            panic!("callback error was not returned intact");
        };
        assert!(std::rc::Rc::ptr_eq(&error, &original));
        assert_eq!(calls, stop);
    }
}

#[test]
fn invalid_derivatives_stop_at_the_first_bad_stage() {
    for invalid in [vec![], vec![1.0, 2.0], vec![f64::NAN], vec![f64::INFINITY]] {
        let mut calls = 0;
        let result = try_ode_solution_points(
            |state| {
                calls += 1;
                Ok::<_, ()>(if calls == 3 {
                    invalid.clone()
                } else {
                    vec![-state[0]]
                })
            },
            &[1.0],
            1.0,
            0.01,
            tune(),
            SamplingBudget::DEFAULT,
        );
        assert!(matches!(result, Err(OdeError::Field(_))));
        assert_eq!(calls, 3);
    }
}

#[test]
fn invalid_controls_and_scalar_output_budget_precede_callbacks() {
    let invalid = [
        (vec![1.0], 0.0, 0.01, 1000),
        (vec![1.0], 1.0, 0.0, 1000),
        (vec![1.0], 1.0, -0.01, 1000),
        (vec![1.0], f64::INFINITY, 0.01, 1000),
        (vec![f64::NAN], 1.0, 0.01, 1000),
        (vec![], 1.0, 0.01, 1000),
        (vec![1.0; 6], 1.0, 0.01, 599),
    ];
    for (initial, time, dt, budget) in invalid {
        let result = try_ode_solution_points(
            |_| -> Result<Vec<f64>, ()> { panic!("invalid request evaluated its callback") },
            &initial,
            time,
            dt,
            tune(),
            SamplingBudget::new(budget),
        );
        assert!(matches!(result, Err(OdeError::Field(_))));
    }
}
