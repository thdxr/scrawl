#!/usr/bin/env python3
# /// script
# dependencies = ["pillow"]
# ///
"""Side-by-side labelled clips and frame strips from `scrawl lab stack` recordings.

    uv run perf/stack/compose.py OUT.mp4 A.mov:Label B.mov:Label … [--width 1152] [--slow 2]
        [--strip OUT.png --strip-at 3.4 --strip-len 0.6 --crop x,y,w,h]

Every recording starts on the lab's own clock (the recorder starts 0.95 s before the first step),
so the clips line up at t=0. Writes H.264 High, yuv420p, +faststart, 60 fps.
"""

import argparse
import subprocess
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont


def duration(path: str) -> float:
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", path],
        check=True, capture_output=True, text=True,
    ).stdout
    return float(out.strip())


def size(path: str) -> tuple[int, int]:
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0", path],
        check=True, capture_output=True, text=True,
    ).stdout
    w, h = (int(v) for v in out.strip().split(","))
    return w, h


def label(text: str, path: Path, w: int, h: int, scale: float) -> None:
    im = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    font = ImageFont.truetype("/System/Library/Fonts/SFNS.ttf", round(22 * scale))
    pad = round(10 * scale)
    tw = d.textlength(text, font=font)
    x, y = round(14 * scale), h - round(52 * scale)
    d.rounded_rectangle((x, y, x + tw + 2 * pad, y + round(36 * scale)), round(9 * scale), fill=(255, 255, 255, 235))
    d.text((x + pad, y + round(5 * scale)), text, font=font, fill=(17, 17, 19, 255))
    im.save(path)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("inputs", nargs="+", help="movie:label")
    ap.add_argument("--width", type=int, default=1152, help="width of each side")
    ap.add_argument("--slow", type=float, default=1.0, help="play this many times slower")
    ap.add_argument("--start", type=float, default=0.0)
    ap.add_argument("--length", type=float, default=0.0)
    ap.add_argument("--crop", default=None, help="x,y,w,h in source pixels, applied before scaling")
    ap.add_argument("--strip", default=None, help="also write a frame strip here")
    ap.add_argument("--strip-at", type=float, default=0.0)
    ap.add_argument("--strip-len", type=float, default=0.6)
    ap.add_argument("--strip-frames", type=int, default=8)
    a = ap.parse_args()

    items = [s.rsplit(":", 1) for s in a.inputs]
    src_w, src_h = size(items[0][0])
    crop = [int(v) for v in a.crop.split(",")] if a.crop else [0, 0, src_w, src_h]
    w = a.width - a.width % 2
    h = round(crop[3] * w / crop[2])
    h -= h % 2
    length = a.length or min(duration(p) for p, _ in items) - a.start
    tmp = Path(tempfile.mkdtemp())
    scale = w / 1152 * 1.4

    inputs, graph = [], ""
    for i, (path, text) in enumerate(items):
        png = tmp / f"label{i}.png"
        label(text, png, w, h, scale)
        inputs += ["-i", path]
        graph += (
            f"[{i}:v]trim=start={a.start:.3f}:duration={length:.3f},setpts=(PTS-STARTPTS)*{a.slow},"
            f"crop={crop[2]}:{crop[3]}:{crop[0]}:{crop[1]},scale={w}:{h}:flags=lanczos,fps=60[v{i}];"
        )
    n = len(items)
    for i in range(n):
        inputs += ["-i", str(tmp / f"label{i}.png")]
        graph += f"[v{i}][{n + i}:v]overlay=0:0[l{i}];"
    if n == 1:
        graph += "[l0]null[out]"
    else:
        for i in range(n - 1):
            graph += f"[l{i}]pad={w + 8}:{h}:0:0:color=0x0b0b0c[p{i}];"
        graph += "".join(f"[p{i}]" for i in range(n - 1)) + f"[l{n - 1}]hstack=inputs={n}[out]"
    enc = ["-c:v", "libx264", "-profile:v", "high", "-pix_fmt", "yuv420p", "-movflags", "+faststart", "-crf", "18", "-an"]
    Path(a.out).parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(["ffmpeg", "-v", "error", "-y", *inputs, "-filter_complex", graph, "-map", "[out]", "-r", "60", *enc, a.out], check=True)
    print(a.out)

    if a.strip:
        rows = []
        k = a.strip_frames
        sw = 300
        for i, (path, text) in enumerate(items):
            png = tmp / f"row{i}.png"
            step = a.strip_len / max(1, k - 1)
            subprocess.run(
                [
                    "ffmpeg", "-v", "error", "-y", "-ss", f"{a.strip_at:.3f}", "-t", f"{a.strip_len + step:.3f}", "-i", path,
                    "-vf", f"fps={1 / step:.4f},crop={crop[2]}:{crop[3]}:{crop[0]}:{crop[1]},scale={sw}:-2,tile={k}x1:padding=4:color=0x0b0b0c",
                    "-frames:v", "1", str(png),
                ],
                check=True,
            )
            rows.append((text, Image.open(png).convert("RGB")))
        gutter = 34
        strip = Image.new("RGB", (rows[0][1].width, sum(r.height + gutter for _, r in rows)), (11, 11, 12))
        d = ImageDraw.Draw(strip)
        font = ImageFont.truetype("/System/Library/Fonts/SFNS.ttf", 18)
        y = 0
        for text, r in rows:
            d.text((6, y + 7), f"{text}   ({a.strip_at:.2f}–{a.strip_at + a.strip_len:.2f} s, every {a.strip_len / max(1, k - 1) * 1000:.0f} ms)", font=font, fill=(220, 220, 225))
            strip.paste(r, (0, y + gutter))
            y += r.height + gutter
        strip.save(a.strip)
        print(a.strip)


if __name__ == "__main__":
    main()
