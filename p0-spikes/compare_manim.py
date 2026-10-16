"""Compare a rustim render against the Manim reference frame, pixel by pixel."""
import sys
import numpy as np
from PIL import Image


def load(path):
    im = Image.open(path).convert("RGBA")
    return np.asarray(im).astype(np.int16)


def bbox_of_nonbackground(a, bg, tol=8):
    """Bounding box of pixels that differ from the background colour."""
    diff = np.abs(a[..., :3] - np.array(bg)).max(axis=2) > tol
    ys, xs = np.nonzero(diff)
    if len(xs) == 0:
        return None
    return int(xs.min()), int(ys.min()), int(xs.max()), int(ys.max())


def main(mine_path, ref_path):
    a = load(mine_path)
    b = load(ref_path)
    print(f"mine : {mine_path}  shape={a.shape}")
    print(f"manim: {ref_path}  shape={b.shape}")
    if a.shape != b.shape:
        print("!! size mismatch")
        return 1

    bg = (0x12, 0x12, 0x12)
    ba = bbox_of_nonbackground(a, bg)
    bb = bbox_of_nonbackground(b, bg)
    print(f"content bbox  mine={ba}  manim={bb}")

    d = np.abs(a[..., :3] - b[..., :3]).max(axis=2)
    for t in (0, 2, 8, 16, 32, 64):
        frac = float((d > t).mean())
        print(f"  pixels differing by > {t:>3}: {frac*100:7.3f}%   ({int((d>t).sum())} px)")
    print(f"  mean abs channel error: {d.mean():.3f}   max: {int(d.max())}")

    # Where the disagreement is, so a systematic offset is distinguishable from
    # an anti-aliasing difference.
    ys, xs = np.nonzero(d > 32)
    if len(xs):
        print(f"  >32 disagreement bbox: x {xs.min()}..{xs.max()}  y {ys.min()}..{ys.max()}")
        # Column/row histograms: a pure shift shows as two dense edges.
        col = np.bincount(xs, minlength=a.shape[1])
        row = np.bincount(ys, minlength=a.shape[0])
        print(f"  busiest columns: {np.argsort(col)[-6:][::-1].tolist()}")
        print(f"  busiest rows   : {np.argsort(row)[-6:][::-1].tolist()}")

    # Sample the same probe points in both, which is what actually pins colour
    # and placement rather than aggregate statistics.
    probes = {
        "background corner": (5, 5),
        "b square (yellow)": (450, 350),
        "a square (blue)": (640, 540),
        "triangle (bright red)": (600, 400),
        "c square over bg (dark red)": (760, 250),
    }
    print("probe points (x, y): mine vs manim")
    for name, (x, y) in probes.items():
        pa = tuple(int(v) for v in a[y, x][:3])
        pb = tuple(int(v) for v in b[y, x][:3])
        flag = "  " if max(abs(u - v) for u, v in zip(pa, pb)) <= 6 else "!!"
        print(f"  {flag} {name:<30} mine={pa} manim={pb}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1], sys.argv[2]))
