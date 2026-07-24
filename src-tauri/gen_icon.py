#!/usr/bin/env python3
"""Generate a 1024x1024 RGBA PNG app icon for Scope (pure stdlib, no Pillow).

Design: rounded-square with a blue->violet vertical gradient, a white
magnifying-glass ring (the "scope"/finder motif) with a small activity/heartbeat
line inside it (the activity-monitor motif), and a diagonal handle.
"""
import math
import struct
import zlib

S = 1024
# RGBA framebuffer
buf = bytearray(S * S * 4)


def put(x, y, r, g, b, a):
    if x < 0 or y < 0 or x >= S or y >= S:
        return
    i = (y * S + x) * 4
    # alpha-over compositing onto existing pixel
    da = buf[i + 3] / 255.0
    sa = a / 255.0
    out_a = sa + da * (1 - sa)
    if out_a <= 0:
        return
    for k, sc in enumerate((r, g, b)):
        dc = buf[i + k]
        oc = (sc * sa + dc * da * (1 - sa)) / out_a
        buf[i + k] = max(0, min(255, int(oc + 0.5)))
    buf[i + 3] = max(0, min(255, int(out_a * 255 + 0.5)))


def lerp(a, b, t):
    return a + (b - a) * t


def rounded_rect_alpha(x, y, x0, y0, x1, y1, radius):
    """Coverage (0..1) of a rounded rectangle at pixel (x,y), lightly AA'd."""
    # distance outside the rounded rect
    cx = min(max(x, x0 + radius), x1 - radius)
    cy = min(max(y, y0 + radius), y1 - radius)
    if x < x0 + radius and y < y0 + radius:
        d = math.hypot(x - (x0 + radius), y - (y0 + radius)) - radius
    elif x > x1 - radius and y < y0 + radius:
        d = math.hypot(x - (x1 - radius), y - (y0 + radius)) - radius
    elif x < x0 + radius and y > y1 - radius:
        d = math.hypot(x - (x0 + radius), y - (y1 - radius)) - radius
    elif x > x1 - radius and y > y1 - radius:
        d = math.hypot(x - (x1 - radius), y - (y1 - radius)) - radius
    else:
        d = max(x0 - x, x - x1, y0 - y, y - y1)
    if d <= -1:
        return 1.0
    if d >= 1:
        return 0.0
    return (1 - (d + 1) / 2)


# --- background rounded square with gradient ---
margin = 96
for y in range(S):
    t = y / (S - 1)
    r = int(lerp(0x2B, 0x6A, t))
    g = int(lerp(0x6C, 0x4C, t))
    b = int(lerp(0xFF, 0xFF, t))
    for x in range(S):
        cov = rounded_rect_alpha(x, y, margin, margin, S - margin, S - margin, 200)
        if cov > 0:
            put(x, y, r, g, b, int(255 * cov))


def ring(cx, cy, radius, thickness, r, g, b, a):
    inner = radius - thickness / 2
    outer = radius + thickness / 2
    x0 = int(cx - outer - 2)
    x1 = int(cx + outer + 2)
    y0 = int(cy - outer - 2)
    y1 = int(cy + outer + 2)
    for y in range(y0, y1):
        for x in range(x0, x1):
            d = math.hypot(x - cx, y - cy)
            edge = min(outer - d, d - inner)  # >0 inside the band
            if edge >= 1:
                put(x, y, r, g, b, a)
            elif edge > 0:
                put(x, y, r, g, b, int(a * edge))


def thick_line(x1a, y1a, x2a, y2a, w, r, g, b, a):
    length = math.hypot(x2a - x1a, y2a - y1a)
    steps = int(length * 2)
    for s in range(steps + 1):
        t = s / steps
        cx = lerp(x1a, x2a, t)
        cy = lerp(y1a, y2a, t)
        rad = int(w / 2)
        for dy in range(-rad, rad + 1):
            for dx in range(-rad, rad + 1):
                dd = math.hypot(dx, dy)
                if dd <= w / 2:
                    aa = 1.0 if dd <= w / 2 - 1 else (w / 2 - dd)
                    put(int(cx + dx), int(cy + dy), r, g, b, int(a * aa))


# --- magnifying glass ---
gx, gy, gr = 430, 430, 210
ring(gx, gy, gr, 58, 255, 255, 255, 255)
# handle
hx = gx + gr * math.cos(math.radians(45))
hy = gy + gr * math.sin(math.radians(45))
thick_line(hx, hy, hx + 190, hy + 190, 74, 255, 255, 255, 255)

# --- activity / heartbeat line inside the lens ---
pts = [
    (gx - 150, gy),
    (gx - 70, gy),
    (gx - 30, gy - 90),
    (gx + 20, gy + 95),
    (gx + 70, gy - 40),
    (gx + 100, gy),
    (gx + 150, gy),
]
for i in range(len(pts) - 1):
    (ax, ay), (bx, by) = pts[i], pts[i + 1]
    thick_line(ax, ay, bx, by, 26, 0x8E, 0xF0, 0xC8, 255)


def write_png(path):
    raw = bytearray()
    for y in range(S):
        raw.append(0)  # filter type 0
        raw.extend(buf[y * S * 4:(y + 1) * S * 4])
    comp = zlib.compress(bytes(raw), 9)

    def chunk(typ, data):
        c = struct.pack(">I", len(data)) + typ + data
        return c + struct.pack(">I", zlib.crc32(typ + data) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", S, S, 8, 6, 0, 0, 0)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", ihdr))
        f.write(chunk(b"IDAT", comp))
        f.write(chunk(b"IEND", b""))


write_png("app-icon.png")
print("wrote app-icon.png")
