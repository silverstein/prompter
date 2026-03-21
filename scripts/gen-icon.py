#!/usr/bin/env python3
"""Generate Prompter app icon — clean teleprompter motif."""
import struct, zlib, math, os

def create_png(w, h, pixels):
    def chunk(ctype, data):
        c = ctype + data
        return struct.pack('>I', len(data)) + c + struct.pack('>I', zlib.crc32(c) & 0xffffffff)
    raw = b''
    for y in range(h):
        raw += b'\x00'
        for x in range(w):
            r, g, b, a = pixels[y * w + x]
            raw += struct.pack('BBBB', r, g, b, a)
    return (b'\x89PNG\r\n\x1a\n' +
            chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 6, 0, 0, 0)) +
            chunk(b'IDAT', zlib.compress(raw, 9)) +
            chunk(b'IEND', b''))

def lerp(a, b, t): return int(a + (b - a) * max(0, min(1, t)))
def dist(x, y, cx, cy): return math.sqrt((x-cx)**2 + (y-cy)**2)

def in_rounded_rect(x, y, x0, y0, x1, y1, r):
    if x < x0 or x > x1 or y < y0 or y > y1: return False
    # Check corners
    for cx, cy in [(x0+r, y0+r), (x1-r, y0+r), (x0+r, y1-r), (x1-r, y1-r)]:
        if (x < x0+r or x > x1-r) and (y < y0+r or y > y1-r):
            if dist(x, y, cx, cy) > r: return False
    return True

def generate_icon(size):
    px = [(0,0,0,0)] * (size * size)
    s = size  # shorthand
    m = int(s * 0.06)  # margin
    rad = int(s * 0.21)  # corner radius

    # Palette
    bg_top = (12, 12, 14)
    bg_bot = (22, 22, 26)
    green = (34, 197, 94)
    amber = (245, 158, 11)
    line_dim = (55, 55, 62)
    line_mid = (75, 75, 82)

    for y in range(s):
        for x in range(s):
            if not in_rounded_rect(x, y, m, m, s-m-1, s-m-1, rad):
                continue

            # Background gradient (top to bottom)
            t = (y - m) / max(s - 2*m, 1)
            bg = (lerp(bg_top[0], bg_bot[0], t),
                  lerp(bg_top[1], bg_bot[1], t),
                  lerp(bg_top[2], bg_bot[2], t))
            r, g, b = bg

            cx, cy = s/2, s/2
            thick = max(s * 0.032, 2)
            half = thick / 2

            # ── Four script lines ──
            x_start = s * 0.16
            spacing = s * 0.095
            lines_cfg = [
                # (y_offset, length_ratio, color, alpha)
                (-1.6, 0.38, line_dim, 0.45),    # past
                (-0.5, 0.48, green, 1.0),         # CURRENT — green, longest
                (0.6,  0.36, line_mid, 0.55),     # upcoming
                (1.7,  0.28, line_dim, 0.35),     # far upcoming
            ]

            drawn = False
            for y_off, length_ratio, lc, alpha in lines_cfg:
                ly = cy + spacing * y_off
                x_end = x_start + s * length_ratio

                if abs(y - ly) <= half and x_start <= x <= x_end:
                    # Rounded caps
                    cap_ok = True
                    if x < x_start + half:
                        if dist(x, y, x_start + half, ly) > half + 0.5: cap_ok = False
                    if x > x_end - half:
                        if dist(x, y, x_end - half, ly) > half + 0.5: cap_ok = False
                    if cap_ok:
                        r = lerp(bg[0], lc[0], alpha)
                        g = lerp(bg[1], lc[1], alpha)
                        b = lerp(bg[2], lc[2], alpha)
                        drawn = True
                        break

            if not drawn:
                # ── Amber pause dot next to current line ──
                dot_x = x_start + s * 0.48 + s * 0.04
                dot_y = cy + spacing * (-0.5)
                dot_r = max(s * 0.02, 1.5)
                d = dist(x, y, dot_x, dot_y)
                if d <= dot_r:
                    a = max(0, 1 - d / dot_r * 0.3)
                    r = lerp(bg[0], amber[0], a * 0.85)
                    g = lerp(bg[1], amber[1], a * 0.85)
                    b = lerp(bg[2], amber[2], a * 0.85)
                    drawn = True

            if not drawn:
                # ── Scroll arrow (right side) — simple filled triangle pointing down ──
                arr_cx = s * 0.77
                arr_cy = cy
                arr_w = s * 0.08   # half-width at top
                arr_h = s * 0.10   # height

                arr_top = arr_cy - arr_h * 0.5
                arr_bot = arr_cy + arr_h * 0.5

                if arr_top <= y <= arr_bot:
                    progress = (y - arr_top) / (arr_bot - arr_top)  # 0 at top, 1 at bottom
                    half_width_at_y = arr_w * (1 - progress)  # narrows toward bottom
                    if abs(x - arr_cx) <= half_width_at_y:
                        # Soft edge antialiasing
                        edge_dist = half_width_at_y - abs(x - arr_cx)
                        a = min(1, edge_dist / 1.5) * 0.7
                        r = lerp(bg[0], green[0], a)
                        g = lerp(bg[1], green[1], a)
                        b = lerp(bg[2], green[2], a)

            px[y * s + x] = (r, g, b, 255)

    return px

script_dir = os.path.dirname(os.path.abspath(__file__))
icons_dir = os.path.join(script_dir, '..', 'crates', 'app', 'icons')

for size in [1024, 512, 256, 128, 64, 32]:
    print(f"  {size}x{size}...", end=" ")
    pixels = generate_icon(size)
    name = 'icon.png' if size == 256 else f'{size}x{size}.png'
    with open(os.path.join(icons_dir, name), 'wb') as f:
        f.write(create_png(size, size, pixels))
    print("ok")

print("Icons generated.")
