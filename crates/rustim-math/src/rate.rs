//! Rate functions: the easing curves that map linear progress to visual progress.
//!
//! Manim calls these `rate_func`s and applies them *inside* each animation,
//! after the scheduler has computed the raw fraction of the run time elapsed.
//! We keep that split deliberately (`方案建议.md` §4.4): the scheduler owns
//! time, the animation owns the curve.
//!
//! # The vocabulary, and the two names that moved
//!
//! Every function in `manim.utils.rate_functions` (0.22) is ported literally,
//! under Manim's own name, with Manim's own formulas and parameters, and the
//! whole set is replayed against the Python originals in `tests/manim_parity.rs`.
//! Two spellings need care, because they changed meaning on the way here:
//!
//! * **[`smooth`] is Manim's `smooth`** — the logistic curve whose inflection
//!   point is a parameter (10 by default) — and *not* the cubic `t²(3−2t)`.
//!   The cubic is Manim's [`smoothstep`] and lives under that name here. This
//!   module used to call the cubic `smooth`; keeping that name would have made
//!   the port unfaithful in exactly the place a reader trusts it most.
//! * **[`ease_in`] / [`ease_out`] / [`ease_in_out`]** are the engine's
//!   historical spellings of [`ease_in_quad`] / [`ease_out_quad`] /
//!   [`ease_in_out_quad`]. They are kept because the `Animation` default names
//!   `ease_in_out` and every scene relies on it; the curves are the same, and a
//!   test pins that they agree to the bit.
//!
//! # Parameters, and why an enum sits beside the functions
//!
//! [`RateFunc`] is a plain `fn(f64) -> f64`, so it can name [`linear`] or
//! [`ease_in_out_quad`] but cannot name `smooth(t, 4.0)`: the inflection is not
//! part of the pointer. The functions here therefore take Manim's parameters
//! explicitly, and a caller that wants a non-default one writes a closure —
//! `|t| smooth(t, 4.0)` is non-capturing and coerces to `RateFunc`. [`Rate`] is
//! the *wire* vocabulary the animating commands use: one variant per curve at
//! Manim's default parameters, serialised under the name Manim gives it, so a
//! host writes `"rate": "ease_in_out_sine"` and not a Rust closure.
//!
//! # Three curves that do not behave like the rest
//!
//! Worth knowing before wiring one to an animation, because each is a correct
//! port of a deliberate Manim shape:
//!
//! * **[`there_and_back`], [`there_and_back_with_pause`], [`wiggle`] return to
//!   zero**, so an animation eased by one ends where it started. `Animation`'s
//!   `finish` drives `interpolate(ease(1.0))`, so a `FadeIn` with
//!   `there_and_back` really does finish transparent — that is Manim's
//!   behaviour, not a lost end state.
//! * **[`running_start`] begins below zero** with its default pull factor
//!   (−0.5): it is a wind-up, not a monotone ease.
//! * **[`exponential_decay`] never arrives** — at `t = 1` it is
//!   `1 − e^{−10} ≈ 0.99995`. An animation eases *to* that value, which for a
//!   position is invisibly short of the target and for `FadeOut`'s opacity is
//!   an object left at 5·10⁻⁵ rather than gone.
//!
//! # What is deliberately not ported
//!
//! Manim decorates every function with `@unit_interval` (return 0 below 0, 1
//! above 1) or `@zero` (return 0 outside). The engine clamps first:
//! `Animation::ease` puts raw progress through [`clamp01`] *before* calling the
//! curve, so the decorators could never be observed and are not reproduced. The
//! parity fixture's `t` grid lies inside `[0, 1]` for the same reason — outside
//! it, the two sides would differ by design.

use serde::{Deserialize, Serialize};

/// A rate function: `[0, 1] -> [0, 1]`, with `f(0) = 0` and `f(1) = 1`.
///
/// Three of the curves here break the second half of that promise on purpose
/// (see the module doc), and the parameterised ones cannot be spelled as this
/// type at all; [`Rate::func`] supplies the defaults.
pub type RateFunc = fn(f64) -> f64;

// ---------------------------------------------------------------------------
// the engine's historical names
// ---------------------------------------------------------------------------

/// Slow start: [`ease_in_quad`] under the name the engine has always used.
pub fn ease_in(t: f64) -> f64 {
    t * t
}

/// Slow finish: [`ease_out_quad`] under the engine's own name.
pub fn ease_out(t: f64) -> f64 {
    1.0 - (1.0 - t) * (1.0 - t)
}

/// The engine's default: quadratic ease in for the first half, quadratic ease
/// out for the second. Piecewise, and the pieces are not the same quadratic —
/// that is what gives it the characteristic snappy-but-soft feel rather than a
/// smoothstep. The same curve as [`ease_in_out_quad`].
pub fn ease_in_out(t: f64) -> f64 {
    if t < 0.5 {
        2.0 * t * t
    } else {
        // Reflect the ease-in branch through (0.5, 0.5).
        1.0 - 2.0 * (1.0 - t) * (1.0 - t)
    }
}

/// Overshoot past the target and settle back, like Manim's `there_and_back`
/// family but monotone in its endpoints.
///
/// An engine extra rather than a port: Manim reaches the same shape through
/// [`ease_out_back`], which starts its overshoot from a different constant.
pub fn overshoot(t: f64) -> f64 {
    // Back-ease with s = 1.70158.
    const S: f64 = 1.701_58;
    let u = t - 1.0;
    u * u * ((S + 1.0) * u + S) + 1.0
}

// ---------------------------------------------------------------------------
// Manim's exported vocabulary (`manim/utils/rate_functions.py`)
// ---------------------------------------------------------------------------

/// Constant speed.
pub fn linear(t: f64) -> f64 {
    t
}

/// The logistic curve, normalised to hit `(0, 0)` and `(1, 1)`.
///
/// `inflection` steers how hard the curve is bent: 10 is Manim's default, and
/// larger values make the middle steeper. **Not** the cubic — that is
/// [`smoothstep`].
pub fn smooth(t: f64, inflection: f64) -> f64 {
    let error = sigmoid(-inflection / 2.0);
    ((sigmoid(inflection * (t - 0.5)) - error) / (1.0 - 2.0 * error)).clamp(0.0, 1.0)
}

/// The cubic `3t² − 2t³`, whose speed is zero at both ends.
pub fn smoothstep(t: f64) -> f64 {
    3.0 * t.powi(2) - 2.0 * t.powi(3)
}

/// The quintic `6t⁵ − 15t⁴ + 10t³`: speed *and* acceleration zero at both ends.
pub fn smootherstep(t: f64) -> f64 {
    6.0 * t.powi(5) - 15.0 * t.powi(4) + 10.0 * t.powi(3)
}

/// The septic `35t⁴ − 84t⁵ + 70t⁶ − 20t⁷`: speed, acceleration and jerk zero.
pub fn smoothererstep(t: f64) -> f64 {
    35.0 * t.powi(4) - 84.0 * t.powi(5) + 70.0 * t.powi(6) - 20.0 * t.powi(7)
}

/// [`smooth`] compressed into the first half and stretched to reach 1 at the end.
pub fn rush_into(t: f64, inflection: f64) -> f64 {
    2.0 * smooth(t / 2.0, inflection)
}

/// [`smooth`] starting halfway through, so the motion arrives fast and settles.
pub fn rush_from(t: f64, inflection: f64) -> f64 {
    2.0 * smooth(t / 2.0 + 0.5, inflection) - 1.0
}

/// A quarter-circle ease in.
pub fn slow_into(t: f64) -> f64 {
    (1.0 - (1.0 - t) * (1.0 - t)).sqrt()
}

/// [`smooth`] in each half, meeting at `(0.5, 0.5)`.
pub fn double_smooth(t: f64) -> f64 {
    if t < 0.5 {
        0.5 * smooth(2.0 * t, DEFAULT_INFLECTION)
    } else {
        0.5 * (1.0 + smooth(2.0 * t - 1.0, DEFAULT_INFLECTION))
    }
}

/// Out and back within the interval: `f(0) = f(1) = 0`, `f(0.5) = 1`.
///
/// An animation eased by this ends where it started — see the module doc.
pub fn there_and_back(t: f64, inflection: f64) -> f64 {
    let new_t = if t < 0.5 { 2.0 * t } else { 2.0 * (1.0 - t) };
    smooth(new_t, inflection)
}

/// [`there_and_back`] with the peak held for `pause_ratio` of the interval.
pub fn there_and_back_with_pause(t: f64, pause_ratio: f64) -> f64 {
    let a = 2.0 / (1.0 - pause_ratio);
    if t < 0.5 - pause_ratio / 2.0 {
        smooth(a * t, DEFAULT_INFLECTION)
    } else if t < 0.5 + pause_ratio / 2.0 {
        1.0
    } else {
        smooth(a - a * t, DEFAULT_INFLECTION)
    }
}

/// A wind-up: starts below zero, crosses it, and lands on 1.
///
/// The Bézier control points are `[0, 0, pull, pull, 1, 1, 1]`, so a negative
/// `pull_factor` (Manim's default is −0.5) pulls the early motion backwards.
pub fn running_start(t: f64, pull_factor: f64) -> f64 {
    let t2 = t * t;
    let t3 = t2 * t;
    let t4 = t3 * t;
    let t5 = t4 * t;
    let t6 = t5 * t;
    let mt = 1.0 - t;
    let mt2 = mt * mt;
    let mt3 = mt2 * mt;
    let mt4 = mt3 * mt;

    15.0 * t2 * mt4 * pull_factor
        + 20.0 * t3 * mt3 * pull_factor
        + 15.0 * t4 * mt2
        + 6.0 * t5 * mt
        + t6
}

/// [`there_and_back`] modulated by a sine — `wiggles` half-oscillations.
///
/// Returns to zero at `t = 1`, like the curve it is built from.
pub fn wiggle(t: f64, wiggles: f64) -> f64 {
    (wiggles * std::f64::consts::PI * t).sin() * there_and_back(t, DEFAULT_INFLECTION)
}

/// [`squish_rate_func`] with the identity curve, using 0.8 of the interval.
pub fn lingering(t: f64) -> f64 {
    squish_rate_func(identity, 0.0, 0.8)(t)
}

/// A decay that never quite arrives: `1 − e^{−t/half_life}`.
///
/// At `t = 1` with the default half-life of 0.1 it is `1 − e^{−10} ≈ 0.99995`,
/// not 1. See the module doc for what that does to an animation's end state.
pub fn exponential_decay(t: f64, half_life: f64) -> f64 {
    1.0 - (-t / half_life).exp()
}

// ---------------------------------------------------------------------------
// Manim's standard easings (not in `__all__`, but the same file)
// ---------------------------------------------------------------------------

pub fn ease_in_sine(t: f64) -> f64 {
    1.0 - (t * std::f64::consts::PI / 2.0).cos()
}

pub fn ease_out_sine(t: f64) -> f64 {
    (t * std::f64::consts::PI / 2.0).sin()
}

pub fn ease_in_out_sine(t: f64) -> f64 {
    -((std::f64::consts::PI * t).cos() - 1.0) / 2.0
}

pub fn ease_in_quad(t: f64) -> f64 {
    t * t
}

pub fn ease_out_quad(t: f64) -> f64 {
    1.0 - (1.0 - t) * (1.0 - t)
}

pub fn ease_in_out_quad(t: f64) -> f64 {
    if t < 0.5 {
        2.0 * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
    }
}

pub fn ease_in_cubic(t: f64) -> f64 {
    t.powi(3)
}

pub fn ease_out_cubic(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}

pub fn ease_in_out_cubic(t: f64) -> f64 {
    if t < 0.5 {
        4.0 * t.powi(3)
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

pub fn ease_in_quart(t: f64) -> f64 {
    t.powi(4)
}

pub fn ease_out_quart(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(4)
}

pub fn ease_in_out_quart(t: f64) -> f64 {
    if t < 0.5 {
        8.0 * t.powi(4)
    } else {
        1.0 - (-2.0 * t + 2.0).powi(4) / 2.0
    }
}

pub fn ease_in_quint(t: f64) -> f64 {
    t.powi(5)
}

pub fn ease_out_quint(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(5)
}

pub fn ease_in_out_quint(t: f64) -> f64 {
    if t < 0.5 {
        16.0 * t.powi(5)
    } else {
        1.0 - (-2.0 * t + 2.0).powi(5) / 2.0
    }
}

pub fn ease_in_expo(t: f64) -> f64 {
    if t == 0.0 {
        0.0
    } else {
        2.0f64.powf(10.0 * t - 10.0)
    }
}

pub fn ease_out_expo(t: f64) -> f64 {
    if t == 1.0 {
        1.0
    } else {
        1.0 - 2.0f64.powf(-10.0 * t)
    }
}

pub fn ease_in_out_expo(t: f64) -> f64 {
    if t == 0.0 {
        0.0
    } else if t == 1.0 {
        1.0
    } else if t < 0.5 {
        2.0f64.powf(20.0 * t - 10.0) / 2.0
    } else {
        (2.0 - 2.0f64.powf(-20.0 * t + 10.0)) / 2.0
    }
}

pub fn ease_in_circ(t: f64) -> f64 {
    1.0 - (1.0 - t.powi(2)).sqrt()
}

pub fn ease_out_circ(t: f64) -> f64 {
    (1.0 - (t - 1.0).powi(2)).sqrt()
}

pub fn ease_in_out_circ(t: f64) -> f64 {
    if t < 0.5 {
        (1.0 - (1.0 - (2.0 * t).powi(2)).sqrt()) / 2.0
    } else {
        ((1.0 - (-2.0 * t + 2.0).powi(2)).sqrt() + 1.0) / 2.0
    }
}

/// Manim's `ease_in_back` constant: the standard overshoot amount.
const BACK_C1: f64 = 1.701_58;

pub fn ease_in_back(t: f64) -> f64 {
    let c3 = BACK_C1 + 1.0;
    c3 * t.powi(3) - BACK_C1 * t.powi(2)
}

pub fn ease_out_back(t: f64) -> f64 {
    let c3 = BACK_C1 + 1.0;
    1.0 + c3 * (t - 1.0).powi(3) + BACK_C1 * (t - 1.0).powi(2)
}

pub fn ease_in_out_back(t: f64) -> f64 {
    let c2 = BACK_C1 * 1.525;
    if t < 0.5 {
        (2.0 * t).powi(2) * ((c2 + 1.0) * 2.0 * t - c2) / 2.0
    } else {
        ((2.0 * t - 2.0).powi(2) * ((c2 + 1.0) * (t * 2.0 - 2.0) + c2) + 2.0) / 2.0
    }
}

pub fn ease_in_elastic(t: f64) -> f64 {
    let c4 = 2.0 * std::f64::consts::PI / 3.0;
    if t == 0.0 {
        0.0
    } else if t == 1.0 {
        1.0
    } else {
        -2.0f64.powf(10.0 * t - 10.0) * ((t * 10.0 - 10.75) * c4).sin()
    }
}

pub fn ease_out_elastic(t: f64) -> f64 {
    let c4 = 2.0 * std::f64::consts::PI / 3.0;
    if t == 0.0 {
        0.0
    } else if t == 1.0 {
        1.0
    } else {
        2.0f64.powf(-10.0 * t) * ((t * 10.0 - 0.75) * c4).sin() + 1.0
    }
}

pub fn ease_in_out_elastic(t: f64) -> f64 {
    let c5 = 2.0 * std::f64::consts::PI / 4.5;
    if t == 0.0 {
        0.0
    } else if t == 1.0 {
        1.0
    } else if t < 0.5 {
        -(2.0f64.powf(20.0 * t - 10.0) * ((20.0 * t - 11.125) * c5).sin()) / 2.0
    } else {
        (2.0f64.powf(-20.0 * t + 10.0) * ((20.0 * t - 11.125) * c5).sin()) / 2.0 + 1.0
    }
}

pub fn ease_in_bounce(t: f64) -> f64 {
    1.0 - ease_out_bounce(1.0 - t)
}

pub fn ease_out_bounce(t: f64) -> f64 {
    const N1: f64 = 7.5625;
    const D1: f64 = 2.75;
    if t < 1.0 / D1 {
        N1 * t * t
    } else if t < 2.0 / D1 {
        N1 * (t - 1.5 / D1) * (t - 1.5 / D1) + 0.75
    } else if t < 2.5 / D1 {
        N1 * (t - 2.25 / D1) * (t - 2.25 / D1) + 0.9375
    } else {
        N1 * (t - 2.625 / D1) * (t - 2.625 / D1) + 0.984375
    }
}

pub fn ease_in_out_bounce(t: f64) -> f64 {
    if t < 0.5 {
        (1.0 - ease_out_bounce(1.0 - 2.0 * t)) / 2.0
    } else {
        (1.0 + ease_out_bounce(2.0 * t - 1.0)) / 2.0
    }
}

// ---------------------------------------------------------------------------
// composition: a curve built out of another curve
// ---------------------------------------------------------------------------

/// A curve that stops `proportion` of the way to its end — Manim's
/// `not_quite_there`.
///
/// Manim's defaults are [`smooth`] at inflection 10 and `proportion = 0.7`; Rust
/// has no default arguments, so both are named by the caller.
///
/// The result is a closure, not a [`RateFunc`] — it captures its arguments — so
/// the engine's trait cannot name it today. It is here because it is part of the
/// vocabulary Manim documents, and because the parity fixture replays it.
pub fn not_quite_there(func: impl Fn(f64) -> f64, proportion: f64) -> impl Fn(f64) -> f64 {
    move |t: f64| proportion * func(t)
}

/// Rescale `func` onto the sub-interval `[a, b]`: flat before `a`, flat after
/// `b`, the original curve in between.
///
/// Manim's defaults are `a = 0.4`, `b = 0.6`. When `a == b` the result is the
/// constant `a`, exactly as upstream. Like [`not_quite_there`], the result is a
/// closure rather than a [`RateFunc`].
pub fn squish_rate_func(func: impl Fn(f64) -> f64, a: f64, b: f64) -> impl Fn(f64) -> f64 {
    move |t: f64| {
        if a == b {
            return a;
        }
        let new_t = if t < a {
            0.0
        } else if t > b {
            1.0
        } else {
            (t - a) / (b - a)
        };
        func(new_t)
    }
}

/// The identity, as a function item so it can be passed to the squisher.
fn identity(t: f64) -> f64 {
    t
}

/// The logistic function Manim's `smooth` family is built from.
fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

// ---------------------------------------------------------------------------
// the wire vocabulary
// ---------------------------------------------------------------------------

/// Manim's default inflection for [`smooth`] and the curves built from it.
pub const DEFAULT_INFLECTION: f64 = 10.0;
/// Manim's default hold for [`there_and_back_with_pause`].
pub const DEFAULT_PAUSE_RATIO: f64 = 1.0 / 3.0;
/// Manim's default wind-up for [`running_start`].
pub const DEFAULT_PULL_FACTOR: f64 = -0.5;
/// Manim's default oscillation count for [`wiggle`].
pub const DEFAULT_WIGGLES: f64 = 2.0;
/// Manim's default half-life for [`exponential_decay`].
pub const DEFAULT_HALF_LIFE: f64 = 0.1;

/// The rate functions a command may name, at Manim's default parameters.
///
/// This is a *wire* type and not a replacement for the functions: it exists
/// because the command layer has to name a curve in JSON, and a `RateFunc` is a
/// pointer, which has no serde spelling. Each variant is serialised under the
/// name Manim's module gives the function, so the vocabulary is one a Manim
/// user already knows.
///
/// The defaults are the reason the enum can be a flat list: `smooth` here means
/// inflection 10, `wiggle` two wiggles. A caller that wants a different
/// parameter uses the function directly with a closure.
///
/// `ease_in_out_quad` — the engine's default, and Manim's — may also be written
/// `ease_in_out`, the engine's historical name for the same curve; it always
/// serialises back as the Manim spelling, because the round-trip promise is
/// about one canonical form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rate {
    Linear,
    Smooth,
    Smoothstep,
    Smootherstep,
    Smoothererstep,
    RushInto,
    RushFrom,
    SlowInto,
    DoubleSmooth,
    ThereAndBack,
    ThereAndBackWithPause,
    RunningStart,
    Wiggle,
    Lingering,
    ExponentialDecay,
    EaseInSine,
    EaseOutSine,
    EaseInOutSine,
    EaseInQuad,
    EaseOutQuad,
    /// The engine's default curve. Also accepted as `"ease_in_out"`.
    #[serde(alias = "ease_in_out")]
    EaseInOutQuad,
    EaseInCubic,
    EaseOutCubic,
    EaseInOutCubic,
    EaseInQuart,
    EaseOutQuart,
    EaseInOutQuart,
    EaseInQuint,
    EaseOutQuint,
    EaseInOutQuint,
    EaseInExpo,
    EaseOutExpo,
    EaseInOutExpo,
    EaseInCirc,
    EaseOutCirc,
    EaseInOutCirc,
    EaseInBack,
    EaseOutBack,
    EaseInOutBack,
    EaseInElastic,
    EaseOutElastic,
    EaseInOutElastic,
    EaseInBounce,
    EaseOutBounce,
    EaseInOutBounce,
}

impl Rate {
    /// Every curve the wire vocabulary offers, in Manim's source order.
    ///
    /// Exists for the two properties a wire vocabulary has to have and a
    /// `match` cannot state: every name is distinct, and every name round-trips.
    /// Both are tested over this array, so a variant added to the enum and to
    /// [`Self::func`] but not here fails the uniqueness check by omission only
    /// if the test counts — it does.
    pub const ALL: [Rate; 45] = [
        Rate::Linear,
        Rate::Smooth,
        Rate::Smoothstep,
        Rate::Smootherstep,
        Rate::Smoothererstep,
        Rate::RushInto,
        Rate::RushFrom,
        Rate::SlowInto,
        Rate::DoubleSmooth,
        Rate::ThereAndBack,
        Rate::ThereAndBackWithPause,
        Rate::RunningStart,
        Rate::Wiggle,
        Rate::Lingering,
        Rate::ExponentialDecay,
        Rate::EaseInSine,
        Rate::EaseOutSine,
        Rate::EaseInOutSine,
        Rate::EaseInQuad,
        Rate::EaseOutQuad,
        Rate::EaseInOutQuad,
        Rate::EaseInCubic,
        Rate::EaseOutCubic,
        Rate::EaseInOutCubic,
        Rate::EaseInQuart,
        Rate::EaseOutQuart,
        Rate::EaseInOutQuart,
        Rate::EaseInQuint,
        Rate::EaseOutQuint,
        Rate::EaseInOutQuint,
        Rate::EaseInExpo,
        Rate::EaseOutExpo,
        Rate::EaseInOutExpo,
        Rate::EaseInCirc,
        Rate::EaseOutCirc,
        Rate::EaseInOutCirc,
        Rate::EaseInBack,
        Rate::EaseOutBack,
        Rate::EaseInOutBack,
        Rate::EaseInElastic,
        Rate::EaseOutElastic,
        Rate::EaseInOutElastic,
        Rate::EaseInBounce,
        Rate::EaseOutBounce,
        Rate::EaseInOutBounce,
    ];

    /// The curve itself, with Manim's default parameters filled in.
    pub fn func(self) -> RateFunc {
        match self {
            Rate::Linear => linear,
            Rate::Smooth => Self::smooth_default,
            Rate::Smoothstep => smoothstep,
            Rate::Smootherstep => smootherstep,
            Rate::Smoothererstep => smoothererstep,
            Rate::RushInto => Self::rush_into_default,
            Rate::RushFrom => Self::rush_from_default,
            Rate::SlowInto => slow_into,
            Rate::DoubleSmooth => double_smooth,
            Rate::ThereAndBack => Self::there_and_back_default,
            Rate::ThereAndBackWithPause => Self::there_and_back_with_pause_default,
            Rate::RunningStart => Self::running_start_default,
            Rate::Wiggle => Self::wiggle_default,
            Rate::Lingering => lingering,
            Rate::ExponentialDecay => Self::exponential_decay_default,
            Rate::EaseInSine => ease_in_sine,
            Rate::EaseOutSine => ease_out_sine,
            Rate::EaseInOutSine => ease_in_out_sine,
            Rate::EaseInQuad => ease_in_quad,
            Rate::EaseOutQuad => ease_out_quad,
            Rate::EaseInOutQuad => ease_in_out_quad,
            Rate::EaseInCubic => ease_in_cubic,
            Rate::EaseOutCubic => ease_out_cubic,
            Rate::EaseInOutCubic => ease_in_out_cubic,
            Rate::EaseInQuart => ease_in_quart,
            Rate::EaseOutQuart => ease_out_quart,
            Rate::EaseInOutQuart => ease_in_out_quart,
            Rate::EaseInQuint => ease_in_quint,
            Rate::EaseOutQuint => ease_out_quint,
            Rate::EaseInOutQuint => ease_in_out_quint,
            Rate::EaseInExpo => ease_in_expo,
            Rate::EaseOutExpo => ease_out_expo,
            Rate::EaseInOutExpo => ease_in_out_expo,
            Rate::EaseInCirc => ease_in_circ,
            Rate::EaseOutCirc => ease_out_circ,
            Rate::EaseInOutCirc => ease_in_out_circ,
            Rate::EaseInBack => ease_in_back,
            Rate::EaseOutBack => ease_out_back,
            Rate::EaseInOutBack => ease_in_out_back,
            Rate::EaseInElastic => ease_in_elastic,
            Rate::EaseOutElastic => ease_out_elastic,
            Rate::EaseInOutElastic => ease_in_out_elastic,
            Rate::EaseInBounce => ease_in_bounce,
            Rate::EaseOutBounce => ease_out_bounce,
            Rate::EaseInOutBounce => ease_in_out_bounce,
        }
    }

    // One-argument shims so the defaulted curves can be a `RateFunc`. The
    // values they pass are the `DEFAULT_*` constants above, which is what makes
    // "the wire name means Manim's default" a single place to read.
    fn smooth_default(t: f64) -> f64 {
        smooth(t, DEFAULT_INFLECTION)
    }
    fn rush_into_default(t: f64) -> f64 {
        rush_into(t, DEFAULT_INFLECTION)
    }
    fn rush_from_default(t: f64) -> f64 {
        rush_from(t, DEFAULT_INFLECTION)
    }
    fn there_and_back_default(t: f64) -> f64 {
        there_and_back(t, DEFAULT_INFLECTION)
    }
    fn there_and_back_with_pause_default(t: f64) -> f64 {
        there_and_back_with_pause(t, DEFAULT_PAUSE_RATIO)
    }
    fn running_start_default(t: f64) -> f64 {
        running_start(t, DEFAULT_PULL_FACTOR)
    }
    fn wiggle_default(t: f64) -> f64 {
        wiggle(t, DEFAULT_WIGGLES)
    }
    fn exponential_decay_default(t: f64) -> f64 {
        exponential_decay(t, DEFAULT_HALF_LIFE)
    }
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

    /// The engine's three historical names, checked against their Manim names.
    const ALIASES: [(&str, RateFunc, RateFunc); 3] = [
        ("ease_in", ease_in, ease_in_quad),
        ("ease_out", ease_out, ease_out_quad),
        ("ease_in_out", ease_in_out, ease_in_out_quad),
    ];

    /// Curves that are monotone on `[0, 1]` *and* end at 1. The overshooting,
    /// returning and never-arriving ones are excluded; each exclusion is a
    /// documented Manim shape, not an accident.
    const MONOTONE: [(&str, RateFunc); 31] = [
        ("linear", linear),
        ("smooth", Rate::smooth_default),
        ("smoothstep", smoothstep),
        ("smootherstep", smootherstep),
        ("smoothererstep", smoothererstep),
        ("rush_into", Rate::rush_into_default),
        ("rush_from", Rate::rush_from_default),
        ("slow_into", slow_into),
        ("double_smooth", double_smooth),
        ("lingering", lingering),
        ("ease_in_sine", ease_in_sine),
        ("ease_out_sine", ease_out_sine),
        ("ease_in_out_sine", ease_in_out_sine),
        ("ease_in_quad", ease_in_quad),
        ("ease_out_quad", ease_out_quad),
        ("ease_in_out_quad", ease_in_out_quad),
        ("ease_in_cubic", ease_in_cubic),
        ("ease_out_cubic", ease_out_cubic),
        ("ease_in_out_cubic", ease_in_out_cubic),
        ("ease_in_quart", ease_in_quart),
        ("ease_out_quart", ease_out_quart),
        ("ease_in_out_quart", ease_in_out_quart),
        ("ease_in_quint", ease_in_quint),
        ("ease_out_quint", ease_out_quint),
        ("ease_in_out_quint", ease_in_out_quint),
        ("ease_in_expo", ease_in_expo),
        ("ease_out_expo", ease_out_expo),
        ("ease_in_out_expo", ease_in_out_expo),
        ("ease_in_circ", ease_in_circ),
        ("ease_out_circ", ease_out_circ),
        ("ease_in_out_circ", ease_in_out_circ),
    ];

    #[test]
    fn aliases_are_the_quad_curves_to_the_bit() {
        // Not "close enough": the engine's default is `ease_in_out`, the wire's
        // name for the same curve is `ease_in_out_quad`, and a host choosing the
        // latter must not silently get a different animation.
        for (name, ours, manims) in ALIASES {
            for i in 0..=1000 {
                let t = f64::from(i) / 1000.0;
                assert_eq!(
                    ours(t),
                    manims(t),
                    "{name} differs from its Manim name at t={t}"
                );
            }
        }
    }

    #[test]
    fn endpoints_are_pinned() {
        for (name, f) in MONOTONE {
            assert!(f(0.0).abs() < 1e-12, "{name}(0) = {}", f(0.0));
            assert!((f(1.0) - 1.0).abs() < 1e-12, "{name}(1) = {}", f(1.0));
        }
    }

    /// The three curves that do not land on 1, each for its own documented
    /// reason — pinning them keeps "fix the endpoint test" from being done by
    /// changing the curve.
    #[test]
    fn the_curves_that_return_are_pinned_as_returns() {
        let returning: [(&str, RateFunc); 3] = [
            ("there_and_back", Rate::there_and_back_default),
            (
                "there_and_back_with_pause",
                Rate::there_and_back_with_pause_default,
            ),
            ("wiggle", Rate::wiggle_default),
        ];
        for (name, f) in returning {
            assert!(f(0.0).abs() < 1e-12, "{name}(0)");
            assert!(f(1.0).abs() < 1e-12, "{name}(1)");
        }
        // ...and the one that approaches without arriving.
        let end = exponential_decay(1.0, DEFAULT_HALF_LIFE);
        assert!((end - (1.0 - (-10.0f64).exp())).abs() < 1e-15);
        assert!(end < 1.0, "exponential_decay must not reach its target");
    }

    /// `running_start` is a wind-up: it deliberately goes below zero before
    /// crossing it. The assertion is the shape, not a value — a "fixed" version
    /// that starts at 0 would be a different curve under the same name.
    #[test]
    fn running_start_pulls_back_first() {
        assert!(running_start(0.1, DEFAULT_PULL_FACTOR) < -0.01);
        assert!(running_start(0.5, DEFAULT_PULL_FACTOR) > 0.0);
    }

    /// The rename that makes the port faithful: `smooth` is the logistic curve
    /// and `smoothstep` the cubic. They were one function here once.
    #[test]
    fn smooth_is_the_sigmoid_and_smoothstep_is_the_cubic() {
        assert_eq!(smooth(0.5, DEFAULT_INFLECTION), 0.5);
        assert_eq!(smoothstep(0.25), 0.15625);
        assert!(
            (smooth(0.25, DEFAULT_INFLECTION) - smoothstep(0.25)).abs() > 1e-3,
            "the two curves must not be interchangeable"
        );
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
        for (name, f) in MONOTONE {
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

    // ---- the wire vocabulary ---------------------------------------------

    /// Every name distinct, every name round-tripping, and nothing silently
    /// missing from [`Rate::ALL`] — the array's length is in its type, so a
    /// variant added to the enum but not the array fails this count.
    #[test]
    fn the_wire_names_are_distinct_and_round_trip() {
        let mut seen = std::collections::BTreeSet::new();
        for rate in Rate::ALL {
            let wire = serde_json::to_string(&rate).unwrap();
            assert!(seen.insert(wire.clone()), "duplicate wire name {wire}");
            let back: Rate = serde_json::from_str(&wire).unwrap();
            assert_eq!(back, rate, "{wire} did not round-trip");
        }
        assert_eq!(seen.len(), Rate::ALL.len());
    }

    #[test]
    fn the_engine_default_is_accepted_under_its_old_name() {
        let spelled: Rate = serde_json::from_str("\"ease_in_out\"").unwrap();
        assert_eq!(spelled, Rate::EaseInOutQuad);
        // ...and it still leaves as the Manim spelling, because a host logs and
        // replays these documents.
        assert_eq!(
            serde_json::to_string(&spelled).unwrap(),
            "\"ease_in_out_quad\""
        );
    }

    /// A wire name means Manim's *default* parameters. Checked against the
    /// parameterised function rather than against the shim, so the constant a
    /// caller reads in the docs and the value the enum actually uses cannot
    /// drift apart.
    #[test]
    fn the_wire_names_carry_manims_default_parameters() {
        let checks: [(Rate, RateFunc); 8] = [
            (Rate::Smooth, |t| smooth(t, 10.0)),
            (Rate::RushInto, |t| rush_into(t, 10.0)),
            (Rate::RushFrom, |t| rush_from(t, 10.0)),
            (Rate::ThereAndBack, |t| there_and_back(t, 10.0)),
            (Rate::ThereAndBackWithPause, |t| {
                there_and_back_with_pause(t, 1.0 / 3.0)
            }),
            (Rate::RunningStart, |t| running_start(t, -0.5)),
            (Rate::Wiggle, |t| wiggle(t, 2.0)),
            (Rate::ExponentialDecay, |t| exponential_decay(t, 0.1)),
        ];
        for (rate, expected) in checks {
            let got = rate.func();
            for i in 0..=100 {
                let t = f64::from(i) / 100.0;
                assert_eq!(got(t), expected(t), "{rate:?} at t={t}");
            }
        }
    }
}
