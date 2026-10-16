"""Measure what Manim's `font_size` means, in Manim's own world units.

Run with the Manim venv's interpreter and `-I` (isolated: no script directory on
`sys.path`, no `PYTHONPATH`), passing the output path as an argument:

    /tmp/manim-venv/bin/python -I tools/manim-fixtures/text_scale.py <out.json>

`方案建议.md` §2.4 records two constants from Manim's *SVG* pipeline
(`TEXT2SVG_ADJUSTMENT_FACTOR = 4.8`, `TEXT_MOB_SCALE_FACTOR = 0.05`). They are
not what this engine needs: they describe how an SVG user unit is scaled after
`svg2paths` has produced one, and the meaning does not survive the trip to a
shaper that works in em units. So the conversion is measured instead, and this
script is the measurement.

What is measured is `Text.` height and width in Manim world units — which is the
`frame_height = 8` space this engine's `Viewport` also uses — for a fixed
reference string, at Manim's default `font_size = 48` and at a few others so the
Rust side can check that the relation really is linear rather than assume it.

The face is named explicitly, and it is the same file this repository bundles:
`assets/fonts/NotoSansCJK-Regular.ttc`. That matters, because a height in world
units is a property of the face as much as of the size — comparing this engine's
Noto Sans CJK SC against Manim's fontconfig default (DejaVu Sans on a bare
Linux) would measure the two fonts, not the two unit systems.
"""

import json
import sys

import manim
from manim import Text

# The reference string. Chosen so that its height is set by glyphs that actually
# reach: `X` has a full cap height and `g` a descender, so the box spans roughly
# the whole ascent-to-descent range rather than a cap alone. CJK is included
# because that is what this engine's bundled face is for, and a face's CJK and
# Latin metrics are not obliged to agree.
REFERENCE = "Xg勾"

# Manim's `Text.height` is the **ink** box — `Text(" ").height == 0`, which no
# line box ever is — and this engine's shaper reports a line box, so the two are
# not comparable directly. This list is how the gap is crossed: `cap_height` is a
# metric in the font's own OS/2 table, `skrifa` reads it in font units, and the
# ink box of a straight-topped capital *is* the cap height. Seven letters rather
# than one, because that is what makes it evidence: they share a cap height and
# nothing else, so a letter whose outline overshoots or whose terminals round off
# shows up as a disagreeing row instead of as a silently wrong constant.
CAP_LETTERS = ["H", "X", "T", "I", "E", "N", "F"]

# Manim's default, plus two more so the Rust side can verify linearity instead of
# trusting it.
FONT_SIZES = [48, 24, 96]

FONT = "Noto Sans CJK SC"


def measure() -> dict:
    rows = []
    for font_size in FONT_SIZES:
        text = Text(REFERENCE, font=FONT, font_size=font_size)
        rows.append(
            {
                "font_size": font_size,
                "height": text.height,
                "width": text.width,
                # Where the block sits vertically in the frame, as a check that
                # the placement is the same convention on both sides. Manim puts
                # `Text` centred on the origin, which is what this engine does.
                "center_y": text.get_center()[1],
            }
        )
    caps = []
    for letter in CAP_LETTERS:
        text = Text(letter, font=FONT, font_size=FONT_SIZES[0])
        caps.append({"letter": letter, "height": text.height})

    return {
        # Recorded rather than remembered: the numbers only mean anything
        # alongside the version and the frame that produced them.
        "cap_letters": caps,
        "manim_version": manim.__version__,
        "frame_height": float(manim.config.frame_height),
        "frame_width": float(manim.config.frame_width),
        "font": FONT,
        "text": REFERENCE,
        "measurements": rows,
    }


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: {sys.argv[0]} <out.json>")
    data = measure()
    with open(sys.argv[1], "w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False, indent=2, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
