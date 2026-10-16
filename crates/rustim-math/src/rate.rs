//! Rate functions: the easing curves that map linear progress to visual progress.
//!
//! Manim calls these `rate_func`s and applies them *inside* each animation,
//! after the scheduler has computed the raw fraction of the run time elapsed.
//! We keep that split deliberately (`方案建议.md` §4.4): the scheduler owns
//! time, the animation owns the curve.

/// A rate function: `[0, 1] -> [0, 1]`, with `f(0) = 0` and `f(1) = 1`.
pub type RateFunc = fn(f64) -> f64;

/// Constant speed.
pub fn linear(t: f64) -> f64 {
    t
}

/// Manim's default: quadratic ease in for the first half, quadratic ease out for
/// the second. Piecewise, and the pieces are not the same quadratic — that is
/// what gives it the characteristic snappy-but-soft feel rather than a smoothstep.
pub fn ease_in_out(t: f64) -> f64 {
    if t < 0.5 {
        2.0 * t * t
    } else {
        // Reflect the ease-in branch through (0.5, 0.5).
        1.0 - 2.0 * (1.0 - t) * (1.0 - t)
    }
}

/// Cubic smoothstep — gentler at both ends than [`ease_in_out`].
pub fn smooth(t: f64) -> f64 {
    t * t * (3.0 - 2.0 * t)
}

/// Slow start.
pub fn ease_in(t: f64) -> f64 {
    t * t
}

/// Slow finish.
pub fn ease_out(t: f64) -> f64 {
    1.0 - (1.0 - t) * (1.0 - t)
}

/// Overshoot past the target and settle back, like Manim's `there_and_back`
/// family but monotone in its endpoints.
pub fn overshoot(t: f64) -> f64 {
    // Back-ease with s = 1.70158.
    const S: f64 = 1.701_58;
    let u = t - 1.0;
    u * u * ((S + 1.0) * u + S) + 1.0
}

/// Clamp `t` into `0..=1`. Scenes can hand us slightly out-of-range progress
/// after a long frame; animations should not have to defend against that.
#[inline]
pub fn clamp01(t: f64) -> f64 {
    t.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [(&str, RateFunc); 6] = [
        ("linear", linear),
        ("ease_in_out", ease_in_out),
        ("smooth", smooth),
        ("ease_in", ease_in),
        ("ease_out", ease_out),
        ("overshoot", overshoot),
    ];

    #[test]
    fn endpoints_are_pinned() {
        for (name, f) in ALL {
            assert!(f(0.0).abs() < 1e-12, "{name}(0) = {}", f(0.0));
            assert!((f(1.0) - 1.0).abs() < 1e-12, "{name}(1) = {}", f(1.0));
        }
    }

    #[test]
    fn ease_in_out_is_symmetric_about_the_midpoint() {
        // The tutorial's sketch splits at t = 0.5; if that split drifts, the
        // curve develops a visible kink, so pin it.
        for i in 0..=100 {
            let t = f64::from(i) / 100.0;
            let a = ease_in_out(t);
            let b = 1.0 - ease_in_out(1.0 - t);
            assert!((a - b).abs() < 1e-12, "asymmetric at t={t}: {a} vs {b}");
        }
    }

    #[test]
    fn ease_in_out_hits_the_halfway_point_at_halfway() {
        assert!((ease_in_out(0.5) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn monotone_curves_never_go_backwards() {
        for (name, f) in ALL {
            // `overshoot` is the one curve allowed to exceed its target, but it
            // still must not reverse in the interval that matters.
            if name == "overshoot" {
                continue;
            }
            let mut prev = f(0.0);
            for i in 1..=1000 {
                let t = f64::from(i) / 1000.0;
                let cur = f(t);
                assert!(cur >= prev - 1e-12, "{name} reversed at t={t}");
                prev = cur;
            }
        }
    }

    #[test]
    fn ease_in_out_eases_in_then_out() {
        // Slower than linear early, faster than linear late.
        assert!(ease_in_out(0.25) < linear(0.25));
        assert!(ease_in_out(0.75) > linear(0.75));
    }

    #[test]
    fn clamp01_bounds() {
        assert_eq!(clamp01(-3.0), 0.0);
        assert_eq!(clamp01(7.0), 1.0);
        assert_eq!(clamp01(0.25), 0.25);
    }
}
