"""Reference scene for the rustim P0 cross-check.

Same figure the tutorial's PythagoreanScene draws: a right triangle with legs
a=3, b=4 and hypotenuse c=5, with a square erected on each side.
3b1b palette per CLAUDE.md: a=blue, b=yellow, c=red, background #121212.
"""
import numpy as np
from manim import *

BLUE_ = ManimColor("#64B5F6")
YELLOW_ = ManimColor("#FFD54F")
RED_ = ManimColor("#EF5350")
BG = ManimColor("#121212")


class Pythagorean(Scene):
    def construct(self):
        self.camera.background_color = BG
        a, b = 3.0, 4.0

        P0 = np.array([0.0, 0.0, 0.0])
        P1 = np.array([a, 0.0, 0.0])
        P2 = np.array([0.0, b, 0.0])

        tri = Polygon(P0, P1, P2, color=RED_, fill_opacity=1.0, stroke_width=0)
        sq_a = Polygon(P0, P1, np.array([a, -a, 0.0]), np.array([0.0, -a, 0.0]),
                       color=BLUE_, fill_opacity=1.0, stroke_width=0)
        sq_b = Polygon(P0, P2, np.array([-b, b, 0.0]), np.array([-b, 0.0, 0.0]),
                       color=YELLOW_, fill_opacity=1.0, stroke_width=0)
        # square on the hypotenuse, erected outwards
        P3 = P1 + np.array([b, a, 0.0])
        P4 = P2 + np.array([b, a, 0.0])
        sq_c = Polygon(P1, P2, P4, P3, color=RED_, fill_opacity=0.45, stroke_width=0)

        figure = VGroup(sq_c, sq_b, sq_a, tri)
        figure.move_to(ORIGIN)
        figure.scale_to_fit_height(6.0)
        self.add(figure)
