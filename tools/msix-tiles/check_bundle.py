"""Refuse a Store package whose tiles are flat colour.

Twice now a submission has been rejected under Microsoft Store policy 10.1.1.11 (On
Device Tiles) because a tile shipped as a single flat colour — the bundler's fallback,
which it writes silently when it has no source for a size. Nothing in the build failed,
nothing warned, and the defect only came back days later as a rejection.

So the build refuses it instead. Run on the finished bundle: a `.msixbundle` is a zip of
`.msix` files, themselves zips, so the tiles are two levels down.

    python3 check_bundle.py path/to/skimrr_0.3.2.0.msixbundle

Exit code 1 on any flat tile, which is what makes a CI job fail.
"""

import io
import sys
import zipfile

from PIL import Image


def tiles(bundle_path: str):
    """Every PNG under Assets/, from every .msix inside the bundle."""
    with zipfile.ZipFile(bundle_path) as bundle:
        packages = [n for n in bundle.namelist() if n.lower().endswith(".msix")]
        if not packages:
            raise SystemExit(f"{bundle_path}: no .msix inside — is this a bundle?")
        for package in packages:
            with zipfile.ZipFile(io.BytesIO(bundle.read(package))) as msix:
                for name in msix.namelist():
                    if name.startswith("Assets/") and name.lower().endswith(".png"):
                        yield package, name, msix.read(name)


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__)
        return 2

    bundle = sys.argv[1]
    checked = flat = 0
    for package, name, data in tiles(bundle):
        image = Image.open(io.BytesIO(data)).convert("RGBA")
        # A real icon has hundreds of colours. Exactly one means the bundler fell back
        # to its placeholder, or composited onto an empty canvas and got nothing.
        colours = len(image.getcolors(maxcolors=10**6) or [])
        checked += 1
        ok = colours > 1
        flat += not ok
        print(
            f"  {name.split('/')[-1]:26} {str(image.size):12} colours={colours:5}"
            f"  {'ok' if ok else '<-- FLAT, would be rejected'}"
        )

    if checked == 0:
        print(f"{bundle}: no tiles found under Assets/ — nothing was verified")
        return 1

    print(f"\n{checked} tiles checked, {flat} flat")
    return 1 if flat else 0


if __name__ == "__main__":
    raise SystemExit(main())
