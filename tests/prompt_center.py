# Prints the offset of the password prompt from the center of a screenshot:
#
#     dx dy
#
# The prompt is each pixel with another color than the top left pixel (the
# color of the lock surface). With no such pixel, the script prints "none".
#
# Usage: prompt_center.py <png>
import sys

from PIL import Image

image = Image.open(sys.argv[1]).convert("RGB")
width, height = image.size
background = image.getpixel((0, 0))
pixels = image.load()
xs, ys = [], []
for y in range(height):
    for x in range(width):
        if pixels[x, y] != background:
            xs.append(x)
            ys.append(y)
if not xs:
    print("none")
else:
    dx = (min(xs) + max(xs)) / 2 - width / 2
    dy = (min(ys) + max(ys)) / 2 - height / 2
    print(f"{dx:.0f} {dy:.0f}")
