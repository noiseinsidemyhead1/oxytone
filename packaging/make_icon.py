"""Icône d'oxytone (même dessin que `app_icon` d'oxytone-gui) : disque dégradé
rose-violet percé au centre, en .ico multi-tailles (Windows) et .png (Linux).

Usage : python packaging/make_icon.py
"""
from pathlib import Path

from PIL import Image

HERE = Path(__file__).resolve().parent
SUPERSAMPLE = 4


def draw(size: int) -> Image.Image:
    big = size * SUPERSAMPLE
    img = Image.new("RGBA", (big, big))
    px = img.load()
    center = (big - 1) / 2
    for y in range(big):
        t = y / big
        color = (int(255 - 60 * t), int(60 + 10 * t), int(110 + 110 * t))
        for x in range(big):
            dx, dy = x - center, y - center
            d = (dx * dx + dy * dy) ** 0.5 / center
            if 0.2 <= d <= 1.0:
                px[x, y] = (*color, 255)
    return img.resize((size, size), Image.LANCZOS)


def main() -> None:
    sizes = [16, 24, 32, 48, 64, 128, 256]
    images = [draw(s) for s in sizes]
    images[-1].save(
        HERE / "windows" / "oxytone.ico",
        sizes=[(s, s) for s in sizes],
        append_images=images[:-1],
    )
    images[-1].save(HERE / "linux" / "oxytone.png")
    print("icônes écrites")


if __name__ == "__main__":
    main()
