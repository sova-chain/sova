#!/usr/bin/env python3
"""Rebuild the repository's generated brand images (stdlib only).

    python3 brand/build-images.py

Writes, next to this script:

  social-preview.png  1280x640, the GitHub repository social preview
                      (Settings > General > Social preview; UI only, no API).
  avatar-500.png      500x500, the sova-chain GitHub org avatar (UI only),
                      from logo/sova-mark-gold-square.svg.

Sources are all in the repo: the wordmark in brand/logo/, the site's fonts
in site/public/fonts/ (Newsreader, Roboto Mono; OFL, licenses alongside).
The preview is laid out as an HTML page with the fonts and wordmark inlined,
then screenshotted by headless Chrome/Chromium. The avatar is rasterized
from the SVG by rsvg-convert (librsvg).

Tools: Chrome or Chromium (set CHROME=/path/to/binary if it is not found)
and rsvg-convert (`brew install librsvg`, `apt install librsvg2-bin`).
"""

import base64
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
FONTS = ROOT / "site" / "public" / "fonts"

WORDMARK = HERE / "logo" / "sova-wordmark-gold.svg"
MARK_SQUARE = HERE / "logo" / "sova-mark-gold-square.svg"
PREVIEW_OUT = HERE / "social-preview.png"
AVATAR_OUT = HERE / "avatar-500.png"

W, H = 1280, 640
AVATAR = 500

# Copy: the positioning line (site/src/data/sova.ts EDGE_LINE) and the
# three properties the site leads with.
# The break keeps "pool." from sitting alone on the second line.
TAGLINE = "The programmable edge<br>of the shielded pool."
SUBLINE = "An EVM chain that can see Zcash."
PROPS = ["permissionless", "oracle-less", "self-custody"]
FOOT_LEFT = "github.com/sova-chain/sova"
FOOT_RIGHT = "sova.io"

PAGE = """<!doctype html>
<html><head><meta charset="utf-8"><style>
@font-face {{ font-family: 'Newsreader'; font-weight: 200 800; font-display: block;
  src: url(data:font/woff2;base64,{serif}) format('woff2'); }}
@font-face {{ font-family: 'Roboto Mono'; font-weight: 100 700; font-display: block;
  src: url(data:font/woff2;base64,{mono}) format('woff2'); }}
:root {{ --ink:#1c1914; --pit:#12100c; --rule:#3a342a; --paper:#fbf3dc;
  --ash:#b3a993; --ash-dim:#8f8672; --gold:#f4b728; --gold-deep:#b0841d; }}
* {{ box-sizing: border-box; margin: 0; }}
html, body {{ width: {w}px; height: {h}px; overflow: hidden; }}
body {{ background: var(--ink); color: var(--paper);
  font-family: 'Newsreader', serif; -webkit-font-smoothing: antialiased; }}
.frame {{ position: absolute; inset: 28px; border: 1px solid var(--rule);
  padding: 0 72px; display: flex; flex-direction: column; justify-content: center; }}
.bar {{ position: absolute; left: 0; right: 0; top: 0; height: 40px;
  border-bottom: 1px solid var(--rule); display: flex; align-items: center;
  padding: 0 22px; gap: 9px; font: 400 15px 'Roboto Mono', monospace; color: var(--ash-dim); }}
.dot {{ width: 10px; height: 10px; border-radius: 50%; border: 1px solid var(--rule); }}
.bar .cmd {{ margin-left: 12px; }}
.bar .cmd b {{ color: var(--gold); font-weight: 400; }}
.mark {{ width: 400px; height: auto; display: block; margin-top: 30px; }}
h1 {{ margin-top: 44px; font-weight: 400; font-size: 64px; line-height: 1.06;
  letter-spacing: -0.01em; font-variation-settings: 'opsz' 72; }}
.sub {{ margin-top: 20px; font: 400 25px/1.3 'Roboto Mono', monospace; color: var(--ash); }}
.props {{ margin-top: 16px; font: 500 21px 'Roboto Mono', monospace; color: var(--gold); }}
.props span + span::before {{ content: " \\00b7  "; color: var(--gold-deep); }}
.foot {{ position: absolute; left: 0; right: 0; bottom: 0; height: 46px;
  border-top: 1px solid var(--rule); display: flex; justify-content: space-between;
  align-items: center; padding: 0 22px; font: 400 16px 'Roboto Mono', monospace; color: var(--ash-dim); }}
.foot .cursor {{ display: inline-block; width: 10px; height: 18px; background: var(--gold);
  vertical-align: -3px; margin-left: 6px; }}
</style></head><body>
<div class="frame">
  <div class="bar"><span class="dot"></span><span class="dot"></span><span class="dot"></span>
    <span class="cmd"><b>$</b> cat README</span></div>
  <img class="mark" alt="Sova" src="data:image/svg+xml;base64,{wordmark}">
  <h1>{tagline}</h1>
  <p class="sub">{subline}</p>
  <p class="props">{props}</p>
  <div class="foot"><span>{foot_left}<span class="cursor"></span></span><span>{foot_right}</span></div>
</div>
</body></html>
"""


def b64(path: Path) -> str:
    return base64.b64encode(path.read_bytes()).decode("ascii")


def find_chrome() -> str:
    env = os.environ.get("CHROME")
    if env:
        return env
    for name in ("google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "chrome"):
        found = shutil.which(name)
        if found:
            return found
    mac = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
    if Path(mac).exists():
        return mac
    sys.exit("build-images: Chrome/Chromium not found; set CHROME=/path/to/binary")


def png_size(path: Path) -> tuple[int, int]:
    head = path.read_bytes()[:24]
    if head[:8] != b"\x89PNG\r\n\x1a\n":
        sys.exit(f"build-images: {path} is not a PNG")
    return struct.unpack(">II", head[16:24])


def check(path: Path, want: tuple[int, int]) -> None:
    got = png_size(path)
    if got != want:
        sys.exit(f"build-images: {path.name} is {got[0]}x{got[1]}, want {want[0]}x{want[1]}")
    print(f"wrote {path.relative_to(ROOT)} ({got[0]}x{got[1]}, {path.stat().st_size} bytes)")


def build_preview() -> None:
    html = PAGE.format(
        w=W,
        h=H,
        serif=b64(FONTS / "newsreader-latin-opsz.woff2"),
        mono=b64(FONTS / "roboto-mono-latin-wght.woff2"),
        wordmark=b64(WORDMARK),
        tagline=TAGLINE,
        subline=SUBLINE,
        props="".join(f"<span>{p}</span>" for p in PROPS),
        foot_left=FOOT_LEFT,
        foot_right=FOOT_RIGHT,
    )
    with tempfile.TemporaryDirectory(prefix="sova-brand-") as tmp:
        page = Path(tmp) / "preview.html"
        page.write_text(html, encoding="utf-8")
        shot = Path(tmp) / "preview.png"
        # Headless Chrome writes the screenshot but does not always exit on
        # macOS, so wait for the file, then stop the browser.
        chrome = subprocess.Popen(
            [
                find_chrome(),
                "--headless=new",
                "--disable-gpu",
                "--no-first-run",
                "--no-default-browser-check",
                f"--user-data-dir={Path(tmp) / 'profile'}",
                "--hide-scrollbars",
                "--force-device-scale-factor=1",
                "--virtual-time-budget=3000",
                f"--window-size={W},{H}",
                f"--screenshot={shot}",
                page.as_uri(),
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        try:
            deadline = time.monotonic() + 60
            last = -1
            while time.monotonic() < deadline:
                size = shot.stat().st_size if shot.exists() else -1
                if size > 0 and size == last:
                    break
                if chrome.poll() is not None and size <= 0:
                    sys.exit("build-images: Chrome exited without a screenshot")
                last = size
                time.sleep(0.5)
            else:
                sys.exit("build-images: timed out waiting for Chrome's screenshot")
        finally:
            if chrome.poll() is None:
                chrome.terminate()
                try:
                    chrome.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    chrome.kill()
                    chrome.wait()
        shutil.copyfile(shot, PREVIEW_OUT)
    check(PREVIEW_OUT, (W, H))


def build_avatar() -> None:
    rsvg = shutil.which("rsvg-convert")
    if not rsvg:
        sys.exit("build-images: rsvg-convert not found (librsvg)")
    subprocess.run(
        [rsvg, "-w", str(AVATAR), "-h", str(AVATAR), "-o", str(AVATAR_OUT), str(MARK_SQUARE)],
        check=True,
    )
    check(AVATAR_OUT, (AVATAR, AVATAR))


if __name__ == "__main__":
    build_preview()
    build_avatar()
