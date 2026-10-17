#!/usr/bin/env python3
"""Dump Manim's own numeric output as JSON, so the Rust port can be checked against it.

Usage:

    /tmp/manim-venv/bin/python -I tools/manim-fixtures/gen.py <output-dir>

This script emits **expected values only** — it contains no assertions. The
comparisons live on the Rust side (`crates/rustim-math/tests/`), so the engine's
test suite never needs Python, Manim, or this venv to run: the JSON is generated
once and checked in.

It also emits the **inputs** it used, under the `inputs` key. That is deliberate:
the Rust test replays the fixture rather than keeping its own copy of the curves
and the t-grid, because two hand-maintained copies of the same table drift, and
a drifted input table makes the two sides agree on a case nobody is testing
while the case that matters goes unrun. One source of truth, in the file.

Why generate rather than transcribe Manim's test suite: several of these routines
carry behaviour that is easy to "improve" while porting and thereby get subtly
wrong — the 10-chord arc length, the homogeneous 4x4 rotation, the quadratic
approximation constants, the closed-vs-open handle-point smoothing. (方案建议.md
risk 7 says exactly this.) Recording what Manim actually does, on inputs we
choose, pins the behaviour instead of the implementation.

Reachability, verified on this machine:
  - /tmp/manim-venv is a Python 3.12 venv with Manim 0.22.0
  - `-I` runs isolated: no script directory on sys.path, no PYTHONPATH, no user
    site-packages. That matters because this repository also holds downloaded,
    untrusted material; the interpreter must not pick code up from next to the
    script or from the current directory.
"""

import json
import platform
import sys
import traceback
from pathlib import Path

import numpy as np

import manim
import manim.utils.bezier as B
import manim.utils.space_ops as S
from manim.mobject.types.vectorized_mobject import VMobject


# --------------------------------------------------------------------------
# collection harness
# --------------------------------------------------------------------------

CASES = {}
FAILURES = []

# The inputs every case was computed from, dumped next to the expected values so
# the Rust side replays this file instead of restating it. Keys are sorted by
# json.dumps; the names here are the ones `crates/rustim-math/tests/` reads.
INPUTS = {}


def jsonable(x):
    """Convert numpy output into something json.dump accepts, losslessly."""
    if isinstance(x, dict):
        return {str(k): jsonable(v) for k, v in x.items()}
    if isinstance(x, (bool, int, float, str)) or x is None:
        return x
    if hasattr(x, "tolist"):  # ndarray, and numpy scalars
        return jsonable(x.tolist())
    if isinstance(x, (list, tuple)):
        return [jsonable(v) for v in x]
    return repr(x)


def case(group, name, fn):
    """Record one expected value. A failing call is reported, not fatal."""
    try:
        value = jsonable(fn())
    except Exception as exc:  # noqa: BLE001 - the point is to survive any of them
        FAILURES.append(f"{group}/{name}: {type(exc).__name__}: {exc}")
        return
    CASES.setdefault(group, {})[name] = value


# --------------------------------------------------------------------------
# inputs — fixed, chosen, and shared with the Rust tests by name
# --------------------------------------------------------------------------

# Cubics in Manim's own point format: a list of 3D points, 4 per curve. Manim's
# world is 3D even for 2D scenes, so z is always 0.0 here and the Rust side
# compares the x/y projection.
CURVES = {
    # A quarter-ish arch. The plain case.
    "arch": [[0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.0], [1.0, 0.0, 0.0]],
    # Legs 3 and 4: the reference scene's own geometry, so a failure here is
    # visible in the golden images too.
    "skew": [[0.0, 0.0, 0.0], [3.0, 0.0, 0.0], [3.0, 4.0, 0.0], [0.0, 4.0, 0.0]],
    # Asymmetric in every coefficient, to catch a swapped control point.
    "wobble": [[-2.0, 1.0, 0.0], [5.0, -3.0, 0.0], [1.0, 2.0, 0.0], [0.0, 0.0, 0.0]],
    # Degenerate: all four points collinear, so the curve is a straight segment
    # traversed non-uniformly. Where an "optimised" arc length shows up as wrong.
    "collinear": [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0], [3.0, 0.0, 0.0]],
    # Cusped: the path doubles back, so a naive "sum of straight segments to the
    # first control point" gets a visibly different answer.
    "cusp": [[0.0, 0.0, 0.0], [1.0, 2.0, 0.0], [2.0, -2.0, 0.0], [3.0, 0.0, 0.0]],
    # Zero-length: every point identical.
    "degenerate": [[1.0, 1.0, 0.0], [1.0, 1.0, 0.0], [1.0, 1.0, 0.0], [1.0, 1.0, 0.0]],
    # y is constant, and x folds back — x(t) = 16t^3 - 24t^2 + 9t is really
    # t(4t - 3)^2, so the curve crosses most x values three times and touches
    # x = 0 twice. Queried at x = 0 it is the one place where only a single
    # axis constrains the answer *and* that axis has a repeated root, so it is
    # what pins whether the port sorts its per-axis roots the way numpy's
    # companion-matrix path does and whether it keeps the duplicate (numpy only
    # deduplicates in `intersect1d`, which never runs for a single axis).
    "folded": [[0.0, 1.0, 0.0], [3.0, 1.0, 0.0], [-2.0, 1.0, 0.0], [1.0, 1.0, 0.0]],
}

TS = [0.0, 0.1, 0.25, 0.5, 0.75, 0.9, 1.0]

# Curves with more than four control points, kept separate from CURVES because
# only some of these routines are defined for any degree. `bezier`,
# `partial_bezier_points`, `split_bezier`, `subdivide_bezier` and `is_closed`
# all dispatch on the degree and have a general branch that four-point curves
# never reach — a port could ship those branches completely wrong and pass
# everything in CURVES. `get_quadratic_approximation_of_cubic` takes four points
# by definition, and `proportions_along_bezier_curve_for_point` bakes the degree
# into a polynomial, so neither belongs here.
HIGH_DEGREE = {
    # Fifth degree: exercises the de Casteljau sweep and the "successive
    # splitting" path in subdivide_bezier (which only runs for N > 4).
    "quintic": [
        [0.0, 0.0, 0.0],
        [1.0, 3.0, 0.0],
        [2.0, -1.0, 0.0],
        [3.0, 4.0, 0.0],
        [4.0, 0.0, 0.0],
        [5.0, 2.0, 0.0],
    ],
    # Fourth degree: the smallest degree past the special cases.
    "quartic": [
        [0.0, 0.0, 0.0],
        [1.0, 2.0, 0.0],
        [2.0, -2.0, 0.0],
        [3.0, 1.0, 0.0],
        [4.0, 0.0, 0.0],
    ],
}

# (a, b) pairs for partial_bezier_points, including the a == b and a > b cases,
# because those are the ones whose behaviour a port is most likely to invent.
#
# The last two are not curves at all: `partial_bezier_points` checks `a == 1`
# and `b == 0` *before* it dispatches on the degree and returns the whole point
# array collapsed onto the last (resp. first) point. A port that only
# implements the portion matrices gets both of these wrong, and every other
# pair in this list passes, so they are here on purpose.
AB_PAIRS = [
    (0.0, 1.0),
    (0.0, 0.5),
    (0.5, 1.0),
    (0.25, 0.75),
    (0.3, 0.3),
    (0.6, 0.2),
    (1.0, 0.5),
    (0.25, 0.0),
]


def curve(name):
    return np.array(CURVES[name], dtype=float)


def _handle_pair(pair):
    """Both handle arrays as a pair of *lists* of points.

    `get_smooth_cubic_bezier_handle_points` returns bare points for the
    two-anchor case and arrays otherwise. Only the container is normalised
    here; the numbers are Manim's, unrounded.
    """
    return [np.atleast_2d(np.asarray(h, dtype=float)).tolist() for h in pair]


# --------------------------------------------------------------------------
# manim.utils.bezier
# --------------------------------------------------------------------------


def collect_bezier():
    INPUTS["curves"] = CURVES
    INPUTS["high_degree"] = HIGH_DEGREE
    INPUTS["ts"] = TS
    INPUTS["ab_pairs"] = AB_PAIRS
    # 1 is the "return the points unchanged" early return in subdivide_bezier.
    INPUTS["cubic_divisions"] = [1, 2, 3, 5]
    INPUTS["high_degree_divisions"] = [1, 2, 3]
    INPUTS["remap_targets"] = [1, 3, 5]
    # The offset added to an on-curve point to make the "off" query. Named here
    # so the Rust test does not have to guess what was perturbed by how much.
    INPUTS["off_curve_offset"] = [0.13, 0.07, 0.0]

    # Degree-generic routines: run on the cubics *and* on the higher-degree
    # curves, so the general branches are pinned too.
    deg_generic = [
        *((name, pts, INPUTS["cubic_divisions"]) for name, pts in CURVES.items()),
        *(
            (name, pts, INPUTS["high_degree_divisions"])
            for name, pts in HIGH_DEGREE.items()
        ),
    ]
    for cname, raw, divisions in deg_generic:
        pts = np.array(raw, dtype=float)

        # bezier() returns a *closure*; the value is bezier(points)(t).
        for t in TS:
            case("bezier", f"{cname}@{t}", lambda pts=pts, t=t: B.bezier(pts)(t))

        for a, b in AB_PAIRS:
            case(
                "partial_bezier_points",
                f"{cname}@{a},{b}",
                lambda pts=pts, a=a, b=b: B.partial_bezier_points(pts, a, b),
            )

        for t in TS:
            case("split_bezier", f"{cname}@{t}", lambda pts=pts, t=t: B.split_bezier(pts, t))

        for n in divisions:
            case(
                "subdivide_bezier",
                f"{cname}@{n}",
                lambda pts=pts, n=n: B.subdivide_bezier(pts, n),
            )

        case("is_closed", cname, lambda pts=pts: B.is_closed(pts))

    # Degree-specific routines: cubics only.
    for cname in CURVES:
        pts = curve(cname)

        # On-curve points must test true, and a point off the curve false. The
        # on-curve sample is taken from bezier() so the two agree by construction.
        #
        # Skipped for the zero-length curve: Manim itself raises
        # `reduce() of empty iterable with no initial value` there, because both
        # routines filter down to the control points that bracket the query point
        # and there are none. That is Manim's behaviour, but it is a crash rather
        # than a value, so there is nothing to pin.
        if len({tuple(p) for p in pts}) > 1:
            midpoint = B.bezier(pts)(0.5)
            case(
                "point_lies_on_bezier",
                f"{cname}@on",
                lambda pts=pts, m=midpoint: B.point_lies_on_bezier(m, pts),
            )
            off = np.array(midpoint) + np.array([0.13, 0.07, 0.0])
            case(
                "point_lies_on_bezier",
                f"{cname}@off",
                lambda pts=pts, o=off: B.point_lies_on_bezier(o, pts),
            )

            case(
                "proportions_along_bezier_curve_for_point",
                f"{cname}",
                lambda pts=pts, m=midpoint: B.proportions_along_bezier_curve_for_point(m, pts),
            )

        if cname == "folded":
            # The curve's own vertex: x = 0 is touched at t = 0 and, doubly, at
            # t = 0.75. Two roots per axis is the case `intersect1d` cannot
            # reach, because y is constant here and so is skipped entirely.
            case(
                "proportions_along_bezier_curve_for_point",
                "folded@vertex",
                lambda pts=pts: B.proportions_along_bezier_curve_for_point(
                    np.array([0.0, 1.0, 0.0]), pts
                ),
            )

        a0, h0, h1, a1 = pts
        case(
            "get_quadratic_approximation_of_cubic",
            cname,
            lambda a0=a0, h0=h0, h1=h1, a1=a1: B.get_quadratic_approximation_of_cubic(
                a0, h0, h1, a1
            ),
        )

    # A path of several curves, for the routines that operate on a whole path.
    path = np.array(
        [
            *CURVES["arch"],
            *[[1.0, 0.0, 0.0], [1.0, -1.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 0.0]],
        ],
        dtype=float,
    )
    case("is_closed", "two_curve_loop", lambda: B.is_closed(path))
    INPUTS["paths"] = {"two_curve_loop": path.tolist()}

    # `is_closed`'s tolerance is `atol + rtol * |start|`, per axis, so it scales
    # with the start point. Every other case here lives within a few units of the
    # origin, where the `rtol` term is at most 1e-4 and a port that dropped it
    # still agrees by luck. Far out it does not: with `start = (1e6, 0)` the
    # tolerance is ~10, so an endpoint a whole unit away is still closed, while an
    # `atol`-only port reports open. The y and z components match exactly, so only
    # the scaled x comparison can decide this case — which is the point.
    rtol_path = np.array(
        [
            [1e6, 0.0, 0.0],
            [1e6 + 1.0, 0.0, 0.0],
            [1e6 + 1.0, 1.0, 0.0],
            [1e6 + 1.0, 0.0, 0.0],
        ],
        dtype=float,
    )
    case("is_closed", "rtol_scaled_tolerance", lambda: B.is_closed(rtol_path))
    INPUTS["paths"]["rtol_scaled_tolerance"] = rtol_path.tolist()

    # bezier_remap changes the number of curves a path is sampled into; it is the
    # primitive Manim's align_points/morph machinery is built on.
    two = np.array([CURVES["arch"], CURVES["skew"]], dtype=float)
    INPUTS["remap_input"] = two.tolist()
    for n in (1, 3, 5):
        case(
            "bezier_remap",
            f"2->{n}",
            lambda two=two, n=n: B.bezier_remap(two, n),
        )

    # Handle-point smoothing: open and closed. Manim caches these (the CP_*_MEMO
    # tables), so a port that recomputes is allowed to differ; a port that
    # computes something else is not.
    #
    # Four anchors is the smallest interesting size, and the only one the
    # recurrences have no special case for. Six is here so a port that got the
    # loop bounds wrong — off by one at either end, or copying the memo's single
    # seeded element in the wrong place — cannot pass by accident.
    anchors_open = np.array(
        [[0.0, 0.0, 0.0], [1.0, 2.0, 0.0], [3.0, 1.0, 0.0], [4.0, 3.0, 0.0]], dtype=float
    )
    anchors_open6 = np.array(
        [
            [0.0, 0.0, 0.0],
            [-1.0, 1.5, 0.0],
            [2.0, 3.0, 0.0],
            [4.0, 1.0, 0.0],
            [5.0, -1.0, 0.0],
            [3.0, -2.0, 0.0],
        ],
        dtype=float,
    )
    anchors_closed = np.array(
        [[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [2.0, 2.0, 0.0], [0.0, 2.0, 0.0]], dtype=float
    )
    anchors_closed6 = np.array(
        [
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [3.0, 1.5, 0.0],
            [1.5, 3.0, 0.0],
            [-0.5, 2.0, 0.0],
            [-1.0, 0.5, 0.0],
            [0.0, 0.0, 0.0],
        ],
        dtype=float,
    )
    INPUTS["anchors"] = {
        "open4": anchors_open.tolist(),
        "open6": anchors_open6.tolist(),
        "closed4": anchors_closed.tolist(),
        "closed6": anchors_closed6.tolist(),
    }
    for name, anchors in (
        ("open4", anchors_open),
        ("open6", anchors_open6),
    ):
        case(
            "get_smooth_open_cubic_bezier_handle_points",
            name,
            lambda a=anchors: B.get_smooth_open_cubic_bezier_handle_points(a),
        )
    for name, anchors in (
        ("closed4", anchors_closed),
        ("closed6", anchors_closed6),
    ):
        case(
            "get_smooth_closed_cubic_bezier_handle_points",
            name,
            lambda a=anchors: B.get_smooth_closed_cubic_bezier_handle_points(a),
        )

    # The dispatcher, including the two anchor counts that never reach either
    # solver: one anchor has no curve at all, two is a straight line whose
    # handles sit at the linear interpolations 1/3 and 2/3 — computed as
    # `(1 - alpha) * start + alpha * end`, which is *not* the same floating
    # point value as `start + alpha * (end - start)`. Getting that wrong is
    # invisible everywhere else in this file.
    #
    # Manim returns bare points for the two-anchor case and one-element arrays
    # for the longer ones. The container is normalised to a pair of lists here
    # so every case in this group has the same shape; the numbers are Manim's.
    for name, anchors in (
        ("one1", np.array([[1.5, -0.5, 0.0]], dtype=float)),
        ("two2", np.array([[0.0, 0.0, 0.0], [3.0, 4.0, 0.0]], dtype=float)),
        ("open4", anchors_open),
        ("open6", anchors_open6),
        ("closed4", anchors_closed),
        ("closed6", anchors_closed6),
    ):
        if name not in INPUTS["anchors"]:
            INPUTS["anchors"][name] = anchors.tolist()
        case(
            "get_smooth_cubic_bezier_handle_points",
            name,
            lambda a=anchors: _handle_pair(B.get_smooth_cubic_bezier_handle_points(a)),
        )


# --------------------------------------------------------------------------
# arc length and critical_point, via VMobject
# --------------------------------------------------------------------------

# These live on VMobject rather than in manim.utils.bezier, and are the two
# routines the proposal calls out by name: the 10-chord arc length and
# critical_point (which morph alignment reads).
#
# Two kinds of path, because they pin different things and only one of them can
# actually see the chord approximation:
#
#   - `CORNERS` go through `set_points_as_corners`, which is what a polygon is
#     built from. It yields *straight* cubics whose control points lie on the
#     segments, so the arc length is exact at every sample count (a 3-4-5
#     polyline reads 12.0 at 2 samples and at 1000). These pin path handling and
#     the anchor/segment layout, and nothing else.
#   - `CUBICS` go through `set_points` with a real control polygon, giving one
#     genuinely curved cubic each. This is where the 9-chord default is visible
#     at all: the arch reads 1.0 at 2 samples, 1.9902960338174542 at 10, and
#     1.999919864980422 at 100. A port that quietly used an analytic arc length
#     would pass every CORNERS case and fail these.
CORNERS = {
    # 3-4-5 right triangle as a polyline: 3 + 5 + 4 = 12, closed.
    "polyline_triangle": [[0.0, 0.0, 0.0], [3.0, 0.0, 0.0], [3.0, 4.0, 0.0], [0.0, 0.0, 0.0]],
    # Three unit segments, open, so the total is 3 rather than 4.
    "unit_square_open": [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]],
}

CUBICS = {
    "cubic_arch": CURVES["arch"],
    "cubic_skew": CURVES["skew"],
    "cubic_wobble": CURVES["wobble"],
    # Cusped, so the *curve* leaves the box its endpoints span: the anchors are
    # (0,0) and (3,0) but the path reaches y = +0.3 and -0.3 in between.
    "cubic_cusp": CURVES["cusp"],
    # Two curves in one path, so the per-curve sum is exercised rather than a
    # single curve's length.
    "cubic_two": list(CURVES["arch"]) + list(CURVES["skew"]),
}


def vm_corners(points):
    """A polygon: each consecutive pair becomes a straight cubic."""
    vm = VMobject()
    vm.set_points_as_corners(np.array(points, dtype=float))
    return vm


def vm_cubic(points):
    """A path of cubic curves, 4 control points each, as Manim stores them."""
    vm = VMobject()
    vm.set_points(np.array(points, dtype=float))
    return vm


# --------------------------------------------------------------------------
# align_points: the morph-alignment core
# --------------------------------------------------------------------------
#
# `align_points` is what makes two differently-shaped paths interpolable at all:
# it grows (or trims) each side until every pair of corresponding subpaths is
# described by the same number of control points. Manim calls it once, from
# `Transform.begin`, and every frame after that is a plain elementwise lerp —
# which is only meaningful if the two arrays are the same length, so the branches
# below are chosen for what they *break*, not for what they look like.
#
# Three of them are reachable only on inputs no ordinary scene would produce, and
# each is a way a plausible port goes quietly wrong:
#
#   - equal point counts return untouched, *before* any normalisation — including
#     the over-eager case of two one-point paths with different points, which
#     upstream flags with a `TODO`;
#   - a path can come back **shorter**, because trailing curves that fold onto the
#     point before them are trimmed (manim#1959), repeatedly;
#   - the predicate that finds subpath boundaries is a numpy `allclose` with its
#     default `rtol`, so the tolerance is *relative*. `rtol_boundary_no_split`
#     sits exactly where an `atol`-only port splits and Manim does not.
#
# `_ARCH` and `_SKEW` come from `CURVES` rather than being repeated here: `skew` is
# the reference scene's own 3-4-5 geometry, so a failure on it is one a golden
# image can also see.

_ARCH = CURVES["arch"]
_SKEW = CURVES["skew"]

# A second subpath, internally chained — its two curves meet at (8,0) — but not
# joined to whatever precedes it, because it starts at (5,0).
_CHAINED = [
    [5.0, 0.0, 0.0], [6.0, 1.0, 0.0], [7.0, 1.0, 0.0], [8.0, 0.0, 0.0],
    [8.0, 0.0, 0.0], [9.0, -1.0, 0.0], [10.0, -1.0, 0.0], [11.0, 0.0, 0.0],
]

# `arch`'s own last point. Repeated, it is a null curve: it folds onto the point
# before it, which is exactly what the trim exists to remove again.
_D = list(_ARCH[3])

# A second curve that *is* chained onto `arch` (it starts where `arch` ends), so
# the pair is one subpath of eight points rather than two of four.
_ARCH_EXTENDED = _ARCH + [[1.0, 0.0, 0.0], [1.0, -1.0, 0.0], [2.0, -1.0, 0.0], [2.0, 0.0, 0.0]]

ALIGN_PAIRS = {
    # Equal counts, so the early return. The only case that pins the
    # zero-difference path through `bezier_remap` as bit-exact.
    "equal_lengths": [_ARCH, _SKEW],
    # Equal counts but *different* points, one point each. The shortcut returns
    # both unchanged; a port without it would answer with two four-point null
    # curves, which is a different array length and so a different fixture.
    "single_vs_single_shortcut": [[[1.0, 1.0, 0.0]], [[2.0, 2.0, 0.0]]],
    # `has_new_path_started`: a lone point becomes a null curve.
    "single_point_bootstrap": [[[2.0, 3.0, 0.0]], _ARCH],
    # `has_no_points`: an empty path becomes four points at the origin.
    "empty_bootstrap": [[], _ARCH],
    # One subpath against two. The shorter side gains a null path built from its
    # *own* last point, not from the other shape's — so a missing subpath morphs
    # into a point rather than into the far side of the other figure.
    "one_subpath_vs_two": [_ARCH, _ARCH + _SKEW],
    "two_subpaths_vs_one": [_ARCH + _SKEW, _ARCH],
    # A real subpath grown from one curve to two — the `1 -> 2` direction, which
    # the existing `bezier_remap` group does not cover.
    "real_subpath_growth": [_ARCH, _ARCH_EXTENDED],
    # A *degenerate* null path grown by one curve: `bezier_remap` over four
    # identical points, where every split factor still has to come out right.
    "null_path_grown": [_ARCH + _CHAINED, _SKEW],
    # The trim: eight points down to four. The result is **shorter** than the
    # input, which is the property a "grow the shorter one" port gets wrong.
    "trailing_null_trim": [_ARCH + [_D] * 4, _SKEW],
    # ...twice over, because the trim is a loop and not an `if`. Twelve to four.
    "trailing_null_double_trim": [_ARCH + [_D] * 4 + [_D] * 4, _SKEW],
    # The trim requires *all four* of the last four points to match the one
    # before them — not merely the last one, and not merely any one. The third of
    # them is (5,5), so a "last point only" port trims here and this one does not.
    "trailing_null_partial_no_trim": [
        [_ARCH[0], _ARCH[1], _ARCH[2], _ARCH[3], _D, [5.0, 5.0, 0.0], _D, _D],
        _SKEW,
    ],
    # Two curves meeting 0.5 apart on a coordinate of 1e6, where `allclose` allows
    # `1e-6 + 1e-5 * 1e6` — about ten units. Manim therefore does *not* split the
    # path in two; an `atol`-only port does, and answers with a different subpath
    # count, a null path and a different array length.
    "rtol_boundary_no_split": [
        [
            [1000000.0, 0.0, 0.0],
            [1000000.0, 1.0, 0.0],
            [1000000.5, 1.0, 0.0],
            [1000000.0, 0.0, 0.0],
            [1000000.5, 0.0, 0.0],
            [1000000.5, -1.0, 0.0],
            [1000001.0, -1.0, 0.0],
            [1000000.5, 0.0, 0.0],
        ],
        _ARCH,
    ],
}


def vm_path(points):
    """A bare path, for `align_points`.

    Deliberately **not** `vm_cubic`: that always calls `set_points`, and
    `set_points(np.array([]))` leaves the array shaped `(0,)` rather than `(0, 3)`,
    which makes `align_points`' own `start_new_path` bootstrap raise as it
    broadcasts. A `VMobject` that was never given points is shaped `(0, 3)` and
    bootstraps to the origin, which is what this case is meant to record.
    """
    vm = VMobject()
    if points:
        vm.set_points(np.array(points, dtype=float))
    return vm


def aligned(p1, p2):
    """Both operands after alignment, in the order they were passed in.

    `align_points` mutates in place and returns `self`, so the Rust port returns
    the pair; the fixture records the same order.
    """
    a, b = vm_path(p1), vm_path(p2)
    a.align_points(b)
    return [a.points.tolist(), b.points.tolist()]


def collect_geometry():
    INPUTS["corner_paths"] = CORNERS
    INPUTS["cubic_paths"] = CUBICS
    # 2 is the sharpest discriminator available: one chord, start to end.
    INPUTS["arc_samples"] = [2, 3, 10, 100]
    INPUTS["critical_directions"] = {
        "right": [1.0, 0.0, 0.0],
        "left": [-1.0, 0.0, 0.0],
        "up": [0.0, 1.0, 0.0],
        "down": [0.0, -1.0, 0.0],
        "up_right": [1.0, 1.0, 0.0],
        "down_left": [-1.0, -1.0, 0.0],
    }
    # The intervals `Create` and `Uncreate` walk: a full range (the copy
    # shortcut), a range inside one curve, a range ending exactly on a
    # curve boundary, a range across curves, and the degenerate empty range
    # `Uncreate` reaches on its last frame.
    INPUTS["partial_bounds"] = [
        [0.0, 0.0],
        [0.0, 0.25],
        [0.0, 0.5],
        [0.25, 0.75],
        [0.1, 0.9],
        [0.0, 1.0],
        [0.75, 1.0],
        [0.5, 0.5],
    ]

    def partial(vm, a, b):
        """`VMobject.pointwise_become_partial` into a fresh object."""
        dst = VMobject()
        dst.pointwise_become_partial(vm, a, b)
        return dst.points

    builders = [(name, vm_corners, pts) for name, pts in CORNERS.items()] + [
        (name, vm_cubic, pts) for name, pts in CUBICS.items()
    ]

    for name, build, pts in builders:
        # No sample argument at all, which is the path most callers take and the
        # one whose default (10) has to be identified rather than assumed.
        case("get_arc_length", name, lambda b=build, p=pts: b(p).get_arc_length())
        for n in INPUTS["arc_samples"]:
            case(
                "get_arc_length",
                f"{name}@samples={n}",
                lambda b=build, p=pts, n=n: b(p).get_arc_length(n),
            )

        # critical_point takes a *direction* and returns the extreme point of the
        # anchors' bounding box along it — anchors, i.e. each curve's start and
        # end points, not the control points and not the sampled curve. The cusp
        # case is what makes that distinction visible: its control points span
        # y in [-2, 2] while its anchors sit on y = 0.
        vm = build(pts)
        for dname, d in INPUTS["critical_directions"].items():
            case(
                "critical_point",
                f"{name}@{dname}",
                lambda vm=vm, d=np.array(d, dtype=float): vm.get_critical_point(d),
            )
        for a, b in INPUTS["partial_bounds"]:
            case(
                "pointwise_partial",
                f"{name}@{a},{b}",
                lambda vm=vm, a=a, b=b: partial(vm, a, b),
            )

    # One case per pair, named after the input, so the Rust test can iterate the
    # input map and read the matching expected value without a second table.
    INPUTS["align_pairs"] = ALIGN_PAIRS
    for name, (p1, p2) in ALIGN_PAIRS.items():
        case("align_points", name, lambda p1=p1, p2=p2: aligned(p1, p2))


# --------------------------------------------------------------------------
# manim.utils.space_ops
# --------------------------------------------------------------------------


def collect_space_ops():
    v_a = np.array([1.0, 2.0, 3.0])
    v_b = np.array([-2.0, 0.5, 4.0])
    v_x = np.array([1.0, 0.0, 0.0])
    v_z = np.array([0.0, 0.0, 1.0])
    INPUTS["vectors"] = {
        "a": v_a.tolist(),
        "b": v_b.tolist(),
        "x": v_x.tolist(),
        "z": v_z.tolist(),
        "minus_z": (-v_z).tolist(),
        "zero": [0.0, 0.0, 0.0],
    }

    case("normalize", "plain", lambda: S.normalize(v_a))
    case("normalize", "already_unit", lambda: S.normalize(v_x))
    case("normalize", "zero_with_fallback", lambda: S.normalize(np.zeros(3), fall_back=v_a))
    case("norm_squared", "plain", lambda: S.norm_squared(v_a))

    case("cross", "ab", lambda: S.cross(v_a, v_b))
    case("cross", "xz", lambda: S.cross(v_x, v_z))
    case("cross2d", "plain", lambda: S.cross2d(np.array([1.0, 2.0]), np.array([3.0, 4.0])))

    case("angle_between_vectors", "ab", lambda: S.angle_between_vectors(v_a, v_b))
    case("angle_between_vectors", "xz", lambda: S.angle_between_vectors(v_x, v_z))
    case("angle_between_vectors", "opposite", lambda: S.angle_between_vectors(v_x, -v_x))

    axes = (
        ("z", [0.0, 0.0, 1.0]),
        ("x", [1.0, 0.0, 0.0]),
        ("diag", [1.0, 1.0, 1.0]),
    )
    angles = (0.0, 0.5, 1.5707963267948966, 3.141592653589793)
    INPUTS["rotation_axes"] = dict(axes)
    INPUTS["rotation_angles"] = list(angles)

    for axis_name, axis in axes:
        for angle in angles:
            case(
                "rotation_matrix",
                f"{axis_name}@{angle}",
                lambda axis=axis, angle=angle: S.rotation_matrix(angle, np.array(axis)),
            )
            # homogeneous=True is the 4x4 form — the "4x4 matrix" the proposal
            # flags as an easy thing to get wrong.
            case(
                "rotation_matrix",
                f"{axis_name}@{angle}@h",
                lambda axis=axis, angle=angle: S.rotation_matrix(
                    angle, np.array(axis), homogeneous=True
                ),
            )
            case(
                "rotation_matrix_transpose",
                f"{axis_name}@{angle}",
                lambda axis=axis, angle=angle: S.rotation_matrix_transpose(
                    angle, np.array(axis)
                ),
            )

    case("z_to_vector", "plain", lambda: S.z_to_vector(v_a))
    case("z_to_vector", "already_z", lambda: S.z_to_vector(v_z))
    case("z_to_vector", "minus_z", lambda: S.z_to_vector(-v_z))
    # The branch where the target direction *is* +x: cross(axis_z, RIGHT) is then
    # zero, and Manim falls back to building the frame from UP instead. Without
    # this case a port that dropped the branch would pass everything else.
    case("z_to_vector", "along_x", lambda: S.z_to_vector(np.array([1.0, 0.0, 0.0])))

    case("midpoint", "3d", lambda: S.midpoint([0.0, 0.0, 0.0], [2.0, 4.0, 6.0]))
    INPUTS["midpoint_pair"] = [[0.0, 0.0, 0.0], [2.0, 4.0, 6.0]]

    # line_intersection takes two lines, each a (point, direction) pair — and the
    # points are 3D, not 2D: passing (2,2) arrays indexes out of bounds at
    # axis 2 rather than being projected.
    lines = {
        "perpendicular": [
            [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]],
            [[1.0, -1.0, 0.0], [0.0, 1.0, 0.0]],
        ],
        "diagonal": [
            [[0.0, 0.0, 0.0], [1.0, 1.0, 0.0]],
            [[0.0, 2.0, 0.0], [1.0, -1.0, 0.0]],
        ],
    }
    INPUTS["lines"] = lines
    for lname, (l1, l2) in lines.items():
        case(
            "line_intersection",
            lname,
            lambda l1=l1, l2=l2: S.line_intersection(
                np.array(l1, dtype=float), np.array(l2, dtype=float)
            ),
        )

    # find_intersection takes four *rays* as (start, end) pairs and returns where
    # each of the first two is crossed by either of the last two.
    rays = [
        [[0.0, 0.0, 0.0], [2.0, 0.0, 0.0]],
        [[1.0, 0.0, 0.0], [-1.0, 0.0, 0.0]],
        [[1.0, -1.0, 0.0], [1.0, 1.0, 0.0]],
        [[0.0, 1.0, 0.0], [0.0, -1.0, 0.0]],
    ]
    INPUTS["rays"] = rays
    case(
        "find_intersection",
        "unit_square_edges",
        lambda rays=rays: S.find_intersection(
            *(np.array(r, dtype=float) for r in rays)
        ),
    )

    # `np.trapezoid(y, x)`, not the classical shoelace sum: the sign comes out
    # inverted relative to the textbook formula, which is why shoelace_direction
    # reads "CW" for a counter-clockwise polygon below.
    square_closed = np.array(
        [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0], [0.0, 0.0]], dtype=float
    )
    square_open = square_closed[:-1].copy()
    INPUTS["square_closed"] = square_closed.tolist()
    INPUTS["square_open"] = square_open.tolist()
    reversed_closed = square_closed[::-1].copy()

    case("shoelace", "ccw", lambda: S.shoelace(square_closed))
    case("shoelace", "cw", lambda: S.shoelace(reversed_closed))
    case("shoelace_direction", "ccw", lambda: S.shoelace_direction(square_closed))
    case("shoelace_direction", "cw", lambda: S.shoelace_direction(reversed_closed))

    # get_winding_number is *not* the standard winding number. It sums the change
    # in each vertex's polar angle about the origin — nothing to do with the
    # polygon's own edges — so the answer is 0 unless the shape encloses the
    # origin. Both shapes below are the same diamond in two spellings; the
    # origin-cornered square is kept as a case precisely because it reads 0.0 in
    # both orientations, which a naive port would "fix" into +/-1.
    diamond_open = np.array([[1.0, 0.0], [0.0, 1.0], [-1.0, 0.0], [0.0, -1.0]])
    diamond_closed = np.vstack([diamond_open, diamond_open[:1]])
    INPUTS["diamond_open"] = diamond_open.tolist()
    INPUTS["diamond_closed"] = diamond_closed.tolist()
    INPUTS["origin_corner_square"] = square_closed.tolist()

    case("get_winding_number", "diamond_open_ccw", lambda: S.get_winding_number(diamond_open))
    case("get_winding_number", "diamond_open_cw", lambda: S.get_winding_number(diamond_open[::-1].copy()))
    case("get_winding_number", "diamond_closed_ccw", lambda: S.get_winding_number(diamond_closed))
    case("get_winding_number", "origin_corner_ccw", lambda: S.get_winding_number(square_closed))

    INPUTS["triangle_3d"] = [[0.0, 0.0, 0.0], [3.0, 0.0, 0.0], [0.0, 3.0, 0.0]]
    case(
        "center_of_mass",
        "triangle",
        lambda: S.center_of_mass(np.array(INPUTS["triangle_3d"], dtype=float)),
    )

    case("cartesian_to_spherical", "plain", lambda: S.cartesian_to_spherical(v_a))
    case(
        "spherical_to_cartesian",
        "roundtrip",
        lambda: S.spherical_to_cartesian(S.cartesian_to_spherical(v_a)),
    )

    case(
        "quaternion_from_angle_axis",
        "z_90",
        lambda: S.quaternion_from_angle_axis(1.5707963267948966, v_z),
    )
    INPUTS["quaternion_mult_pair"] = [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]]
    case(
        "quaternion_mult",
        "identity",
        lambda: S.quaternion_mult(*[np.array(q) for q in INPUTS["quaternion_mult_pair"]]),
    )
    case(
        "rotate_vector",
        "z_90",
        lambda: S.rotate_vector(v_x, 1.5707963267948966, v_z),
    )


# --------------------------------------------------------------------------
# main
# --------------------------------------------------------------------------


def inventory():
    def names(module):
        return sorted(n for n in dir(module) if not n.startswith("_"))

    return {
        "manim.utils.bezier": names(B),
        "manim.utils.space_ops": names(S),
    }


def main(argv):
    if len(argv) != 2:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    outdir = Path(argv[1])
    outdir.mkdir(parents=True, exist_ok=True)

    collect_bezier()
    collect_geometry()
    collect_space_ops()

    # group -> the one file it belongs in. Groups are merged, never written one
    # at a time: each file is written once, or the later group would clobber the
    # earlier ones.
    files = {
        "manim-bezier.json": [
            "bezier",
            "partial_bezier_points",
            "split_bezier",
            "subdivide_bezier",
            "bezier_remap",
            "is_closed",
            "point_lies_on_bezier",
            "proportions_along_bezier_curve_for_point",
            "get_quadratic_approximation_of_cubic",
            "get_smooth_open_cubic_bezier_handle_points",
            "get_smooth_closed_cubic_bezier_handle_points",
            "get_smooth_cubic_bezier_handle_points",
        ],
        "manim-geometry.json": [
            "get_arc_length",
            "critical_point",
            "align_points",
            "pointwise_partial",
        ],
        "manim-space-ops.json": [
            "normalize",
            "norm_squared",
            "cross",
            "cross2d",
            "angle_between_vectors",
            "rotation_matrix",
            "rotation_matrix_transpose",
            "z_to_vector",
            "midpoint",
            "line_intersection",
            "find_intersection",
            "shoelace",
            "shoelace_direction",
            "get_winding_number",
            "center_of_mass",
            "cartesian_to_spherical",
            "spherical_to_cartesian",
            "quaternion_from_angle_axis",
            "quaternion_mult",
            "rotate_vector",
        ],
    }

    missing = sorted(set(CASES) - {g for groups in files.values() for g in groups})
    if missing:
        raise RuntimeError(f"collected groups with no output file: {missing}")

    # Each file carries only the inputs its own groups read, so it stays
    # self-contained and does not grow a pile of tables nothing uses.
    inputs_for = {
        "manim-bezier.json": [
            "curves",
            "high_degree",
            "ts",
            "ab_pairs",
            "cubic_divisions",
            "high_degree_divisions",
            "remap_targets",
            "remap_input",
            "off_curve_offset",
            "paths",
            "anchors",
        ],
        "manim-geometry.json": [
            "corner_paths",
            "cubic_paths",
            "arc_samples",
            "critical_directions",
            "align_pairs",
            "partial_bounds",
        ],
        "manim-space-ops.json": [
            "vectors",
            "rotation_axes",
            "rotation_angles",
            "midpoint_pair",
            "lines",
            "rays",
            "square_closed",
            "square_open",
            "diamond_open",
            "diamond_closed",
            "origin_corner_square",
            "triangle_3d",
            "quaternion_mult_pair",
        ],
    }

    for filename, groups in files.items():
        present = {g: CASES[g] for g in groups if g in CASES}
        want = inputs_for[filename]
        missing_inputs = [k for k in want if k not in INPUTS]
        if missing_inputs:
            raise RuntimeError(f"{filename}: inputs never recorded: {missing_inputs}")
        payload = {
            "generated_by": f"manim {manim.__version__}",
            "python": platform.python_version(),
            "source": "tools/manim-fixtures/gen.py",
            "note": "expected values and the inputs they were computed from; "
            "the assertions are on the Rust side",
            "inventory": inventory(),
            "inputs": {k: INPUTS[k] for k in want},
            "cases": present,
        }
        (outdir / filename).write_text(
            json.dumps(payload, indent=1, sort_keys=True) + "\n", encoding="utf-8"
        )
        total = sum(len(v) for v in present.values())
        print(f"{filename}: {total} cases in {len(present)} groups")

    if FAILURES:
        print("\ncalls that raised (fix the generator, not the port):", file=sys.stderr)
        for f in FAILURES:
            print(f"  {f}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv))
    except Exception:  # noqa: BLE001
        traceback.print_exc()
        sys.exit(1)
