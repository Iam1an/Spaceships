#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
genjet.py -- procedurally generate an F-22 Raptor style stealth fighter as a .glb.

Stdlib only: struct + json.  No Blender, no numpy, no trimesh, no pygltflib.
Run:  python3 spaceships-rs/tools/genjet.py [out.glb]
Default output: public/jet.glb


ORIENTATION -- READ THIS BEFORE CHANGING ANYTHING
-------------------------------------------------
The *game* treats ship-space forward as +Z:  main.js does
    new THREE.Vector3(0, 0, 1).applyQuaternion(ship.quaternion)
to get the nose direction.

The *GLB* is NOT authored in ship space.  ship.js does, for every model it loads:
    model.rotation.y = -Math.PI / 2
which maps model (x, y, z) -> ship (-z, y, x).  So a GLB whose nose is on +X comes
out pointing along ship +Z.  public/spaceship.glb is authored exactly that way (its
body mesh runs out to x=+4.06 and its engine cluster back to x=-4.34), and
public/src/cockpit.js states it outright:

    "Both GLB models face +X and are rotated -PI/2 about Y in ship.js, which maps
     model (x, y, z) -> ship (-z, y, x). That is why ship forward is +Z everywhere
     in main.js."

Therefore this file builds in MODEL space:
    +X = nose (forward)   +Y = up   +Z = pilot's right   -Z = pilot's left
which, after ship.js's rotation, gives ship-space length along +Z, span along X and
height along Y.  Authoring the nose on +Z instead would make the ship fly sideways.


MATERIALS -- also load-bearing
------------------------------
customization.js and ship.js split hull from accent with
    0.2126*r + 0.7152*g + 0.0722*b < 0.35   -> accent
reading THREE.Material.color, which GLTFLoader fills straight from baseColorFactor
in LINEAR space.  So the test is applied to the raw baseColorFactor.  Two materials:
    Hull          0.55 grey-blue  luminance ~0.564  -> above threshold -> hull group
    Accent_Glass  0.11 near-black luminance ~0.109  -> below threshold -> accent group
Node names matter too: isAccentMesh() also matches names containing
cockpit/engine/window/glass.  "Hull" matches none of those; "Accent_Glass" matches
"glass", so the accent group is caught by BOTH mechanisms.  Never rename a hull part
to anything containing those substrings.


SIZE -- note the measurement trap
---------------------------------
The airframe below is authored at a convenient working size (about 10.3 long) and
then scaled/shifted on export by EXPORT_SCALE / EXPORT_SHIFT_X, because the raw
numbers you get from a naive read of public/spaceship.glb are NOT the size that
model renders at:

    raw mesh bounds, node transforms ignored : 8.39 x 4.10 x 8.00
    true bounds, Box3.setFromObject(ship)    : span 5.43, height 1.74, length 5.62

spaceship.glb's nodes carry scales of 0.11 to 0.66, so the meshes are much smaller
in place than their vertex data suggests.  Everything in the game -- the chase
camera (camera.js distance 11 / height 5.6), shipRadius (2.2 * SHIP_SCALE), the
bot's SHIP_RADIUS 3.5, BEAM_SHIP_RADIUS 5.5 -- is tuned against the TRUE numbers.
Building to the raw numbers gives an aircraft ~1.8x oversized, whose collision
sphere covers less than half of it.

So the export defaults reproduce the *relative* change that was asked for (longer,
narrower, flatter) applied to the true envelope:

    exported: length(z) 6.70   span(x) 5.22   height(y) 1.65    ratio 1.28 : 1
    current : length(z) 5.62   span(x) 5.43   height(y) 1.74    ratio 1.03 : 1

Run with --brief-size to emit the literal 10.3 x 8.0 x 2.5 envelope instead.


STYLE
-----
Everything is flat shaded: normals are per-face, and vertices are merged only when
position AND normal agree.  Faceting is the point -- a stealth airframe really is
made of flat panels, so hard edges read as correct rather than as low effort.  Do
not add smoothing.
"""

import json
import math
import os
import struct
import sys

# ---------------------------------------------------------------------------
# vector helpers
# ---------------------------------------------------------------------------

def vsub(a, b):
    return (a[0] - b[0], a[1] - b[1], a[2] - b[2])


def vadd(a, b):
    return (a[0] + b[0], a[1] + b[1], a[2] + b[2])


def vmul(a, s):
    return (a[0] * s, a[1] * s, a[2] * s)


def vcross(a, b):
    return (a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0])


def vdot(a, b):
    return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]


def vlen(a):
    return math.sqrt(vdot(a, a))


def vnorm(a):
    l = vlen(a)
    return (a[0] / l, a[1] / l, a[2] / l) if l > 1e-12 else (0.0, 0.0, 0.0)


def lerp(a, b, t):
    return (a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t)


def centroid(pts):
    n = float(len(pts))
    return (sum(p[0] for p in pts) / n,
            sum(p[1] for p in pts) / n,
            sum(p[2] for p in pts) / n)


def mirror_z(p):
    return (p[0], p[1], -p[2])


def rot_x(p, ang):
    c, s = math.cos(ang), math.sin(ang)
    return (p[0], p[1] * c - p[2] * s, p[1] * s + p[2] * c)


# ---------------------------------------------------------------------------
# triangle accumulation
# ---------------------------------------------------------------------------

HULL = 0
ACCENT = 1
TRIS = {HULL: [], ACCENT: []}
DEGENERATE = [0]
SHELLS = []         # (name, mat, start, end) -- closed shells, checked for orientation


def tri(a, b, c, m=HULL):
    if vlen(vcross(vsub(b, a), vsub(c, a))) < 1e-9:
        DEGENERATE[0] += 1
        return
    TRIS[m].append((a, b, c))


def quad(a, b, c, d, m=HULL):
    tri(a, b, c, m)
    tri(a, c, d, m)


def quad_out(a, b, c, d, want_n, m=HULL):
    """Quad wound so that its normal agrees with want_n."""
    if vdot(vcross(vsub(b, a), vsub(c, a)), want_n) < 0.0:
        quad(a, d, c, b, m)
    else:
        quad(a, b, c, d, m)


class shell(object):
    """Marks a CLOSED shell so report() can verify its normals point outward."""

    def __init__(self, name, m=HULL):
        self.name, self.m = name, m

    def __enter__(self):
        self.start = len(TRIS[self.m])
        return self

    def __exit__(self, *a):
        SHELLS.append((self.name, self.m, self.start, len(TRIS[self.m])))
        return False


def mirrored(fn, *args, **kw):
    """Run fn (which builds only the +Z half) then mirror it across z=0.

    Reflection flips handedness, so every mirrored triangle's winding is reversed
    to keep normals pointing outward.
    """
    marks = {m: len(TRIS[m]) for m in TRIS}
    fn(*args, **kw)
    for m in TRIS:
        for (a, b, c) in list(TRIS[m][marks[m]:]):
            TRIS[m].append((mirror_z(a), mirror_z(c), mirror_z(b)))


# ---------------------------------------------------------------------------
# lofting
#
# Ring convention: vertices ordered CLOCKWISE seen from +X (looking aft), ring
# lists ordered front (largest x) to back.  With that, quad(A[j],B[j],B[k],A[k])
# with A forward and B aft has an outward normal.
# ---------------------------------------------------------------------------

def loft(rings, m=HULL, closed=True, flip=False):
    for i in range(len(rings) - 1):
        A, B = rings[i], rings[i + 1]
        n = len(A)
        for j in (range(n) if closed else range(n - 1)):
            k = (j + 1) % n
            if flip:
                quad(A[j], A[k], B[k], B[j], m)
            else:
                quad(A[j], B[j], B[k], A[k], m)


def cap(ring, want_n, m=HULL):
    """Fan cap wound so its normal points along want_n.  Direction is chosen at
    build time rather than derived by hand -- that removes a whole class of
    inside-out bugs."""
    c = centroid(ring)
    n0 = vcross(vsub(ring[0], c), vsub(ring[1], c))
    fwd = vdot(n0, want_n) >= 0.0
    n = len(ring)
    for j in range(n):
        k = (j + 1) % n
        if fwd:
            tri(c, ring[j], ring[k], m)
        else:
            tri(c, ring[k], ring[j], m)


class Surf(object):
    """A lofted surface addressable in (u, v) so detail can be laid onto it.

    u runs 0..1 along the ring list, v runs 0..1 around the ring (wrapping).
    sgn is +1 when the loft was built unflipped (normals already outward) and -1
    when it was built with flip=True.
    """

    def __init__(self, rings, sgn=1.0):
        self.rings = rings
        self.sgn = sgn

    def pt(self, u, v):
        rings = self.rings
        n = len(rings)
        fu = max(0.0, min(1.0, u)) * (n - 1)
        i = min(int(fu), n - 2)
        tu = fu - i
        r0, r1 = rings[i], rings[i + 1]
        k = len(r0)
        fv = (v % 1.0) * k
        j = int(fv) % k
        tv = fv - int(fv)
        return lerp(lerp(r0[j], r0[(j + 1) % k], tv),
                    lerp(r1[j], r1[(j + 1) % k], tv), tu)

    def du(self, u, v, e=2e-3):
        return vsub(self.pt(min(1.0, u + e), v), self.pt(max(0.0, u - e), v))

    def dv(self, u, v, e=2e-3):
        return vsub(self.pt(u, v + e), self.pt(u, v - e))

    def nrm(self, u, v):
        return vmul(vnorm(vcross(self.du(u, v), self.dv(u, v))), self.sgn)


# --- surface detailing -----------------------------------------------------

def scribe(S, samples, du, dv, m=ACCENT, lift=0.008):
    """A thin strip of `m` laid on a surface: a panel line / seam."""
    left, right = [], []
    for (u, v) in samples:
        n = S.nrm(u, v)
        left.append(vadd(S.pt(u - du, v - dv), vmul(n, lift)))
        right.append(vadd(S.pt(u + du, v + dv), vmul(n, lift)))
    for i in range(len(samples) - 1):
        quad_out(left[i], right[i], right[i + 1], left[i + 1],
                 S.nrm(*samples[i]), m)


# Panel line widths are specified in WORLD units and converted to the local
# parameter step.  Doing it the other way round -- constant width in (u, v) --
# is what makes lines fan out into fat black bars over a tapered wing.
LINE_W = 0.040
SEAM_W = 0.030


def _wid_v(S, u, v, w):
    d = vlen(S.dv(min(max(u, 0.01), 0.99), v)) / 4e-3
    return (w * 0.5) / max(d, 1e-6)


def _wid_u(S, u, v, w):
    d = vlen(S.du(min(max(u, 0.01), 0.99), v)) / 4e-3
    return (w * 0.5) / max(d, 1e-6)


def line_u(S, u0, u1, v, n=8, w=LINE_W, m=ACCENT, lift=0.008):
    """Lengthwise panel line at constant v."""
    dv = _wid_v(S, 0.5 * (u0 + u1), v, w)
    scribe(S, [(u0 + (u1 - u0) * i / float(n), v) for i in range(n + 1)],
           0.0, dv, m, lift)


def line_v(S, u, v0, v1, n=6, w=LINE_W, m=ACCENT, lift=0.008):
    """Chordwise panel line / hinge line at constant u."""
    du = _wid_u(S, u, 0.5 * (v0 + v1), w)
    scribe(S, [(u, v0 + (v1 - v0) * i / float(n)) for i in range(n + 1)],
           du, 0.0, m, lift)


def saw_u(S, u0, u1, v_lo, v_hi, teeth=8, m=ACCENT, lift=0.009, w=SEAM_W):
    """Saw-tooth seam running lengthwise -- the classic low-observable door edge."""
    dv = _wid_v(S, 0.5 * (u0 + u1), v_lo, w)
    pts = [(u0 + (u1 - u0) * i / float(teeth * 2),
            v_lo if (i % 2 == 0) else v_hi) for i in range(teeth * 2 + 1)]
    scribe(S, pts, 0.0, dv, m, lift)


def saw_v(S, u_lo, u_hi, v0, v1, teeth=5, m=ACCENT, lift=0.009, w=SEAM_W):
    """Saw-tooth seam running across the body."""
    du = _wid_u(S, 0.5 * (u_lo + u_hi), 0.5 * (v0 + v1), w)
    pts = [(u_lo if (i % 2 == 0) else u_hi,
            v0 + (v1 - v0) * i / float(teeth * 2)) for i in range(teeth * 2 + 1)]
    scribe(S, pts, du, 0.0, m, lift)


def plate(S, u0, u1, v0, v1, h=0.014, m=HULL, nu=2, nv=2):
    """A raised access panel standing proud of the surface.

    Sits ON the hull, so no hole cutting is needed; the side walls catch the key
    light and read as a crisply scribed panel edge.
    """
    def P(u, v, off):
        return vadd(S.pt(u, v), vmul(S.nrm(u, v), off))

    us = [u0 + (u1 - u0) * i / float(nu) for i in range(nu + 1)]
    vs = [v0 + (v1 - v0) * i / float(nv) for i in range(nv + 1)]
    for i in range(nu):
        for j in range(nv):
            quad_out(P(us[i], vs[j], h), P(us[i + 1], vs[j], h),
                     P(us[i + 1], vs[j + 1], h), P(us[i], vs[j + 1], h),
                     S.nrm(us[i], vs[j]), m)
    for j in range(nv):
        for (u, sgn) in ((u0, -1.0), (u1, 1.0)):
            quad_out(P(u, vs[j], 0.0), P(u, vs[j + 1], 0.0),
                     P(u, vs[j + 1], h), P(u, vs[j], h),
                     vmul(vnorm(S.du(u, vs[j])), sgn), m)
    for i in range(nu):
        for (v, sgn) in ((v0, -1.0), (v1, 1.0)):
            quad_out(P(us[i], v, 0.0), P(us[i + 1], v, 0.0),
                     P(us[i + 1], v, h), P(us[i], v, h),
                     vmul(vnorm(S.dv(us[i], v)), sgn), m)


def door(S, u0, u1, v0, v1, teeth=8, h=0.010, m=HULL):
    """A closed bay door: a shallow raised panel with saw-tooth long seams."""
    plate(S, u0, u1, v0, v1, h, m, 3, 2)
    a = _wid_v(S, 0.5 * (u0 + u1), v0, 0.10)     # tooth amplitude, world units
    saw_u(S, u0, u1, v0 - a, v0 + a * 0.3, teeth, ACCENT, h + 0.006)
    saw_u(S, u0, u1, v1 + a, v1 - a * 0.3, teeth, ACCENT, h + 0.006)
    line_v(S, u0 - 0.004, v0, v1, 4, SEAM_W, ACCENT, h + 0.006)
    line_v(S, u1 + 0.004, v0, v1, 4, SEAM_W, ACCENT, h + 0.006)


# ---------------------------------------------------------------------------
# FUSELAGE
#
# Cross-section is a faceted hexagon: flat bottom, angled lower cheeks flaring
# out and up to a sharp chine at max width, angled upper cheeks converging on a
# flat top deck.  No curves anywhere.
# ---------------------------------------------------------------------------

def droop(x):
    """The nose tapers downward toward the tip -- clear in the side profile."""
    t = max(0.0, min(1.0, (x - 2.30) / 2.60))
    return -0.21 * (t ** 1.7)


def fuse_ring(x, tw, ty, cw, cy, bw, by, ky=None, sy=None, ubulge=0.055, lbulge=0.045):
    """12 point section, clockwise seen from the nose.

    ring index -> v:  0 spine 0.000 | 1 deck edge .083 | 2 upper cheek .167
    3 CHINE .250 | 4 lower cheek .333 | 5 bottom edge .417 | 6 keel .500
    and mirrored back up the -Z side.
    """
    d = droop(x)
    ty += d
    cy += d
    by += d
    ky = (ty if ky is None else ky + d)
    sy = (by if sy is None else sy + d)
    tu = 0.56
    uw, uy = tw + (cw - tw) * tu + ubulge, ty + (cy - ty) * tu
    tl = 0.50
    lw, ly = bw + (cw - bw) * tl + lbulge, by + (cy - by) * tl
    return [
        (x, ky, 0.0), (x, ty, tw), (x, uy, uw), (x, cy, cw),
        (x, ly, lw), (x, by, bw), (x, sy, 0.0),
        (x, by, -bw), (x, ly, -lw), (x, cy, -cw), (x, uy, -uw), (x, ty, -tw),
    ]


# x,      tw,    ty,     cw,    cy,     bw,    by,     ky,     sy
FUSE_TABLE = [
    (4.90, 0.012, 0.008, 0.020, 0.000, 0.012, -0.012, 0.010, -0.014),
    (4.66, 0.030, 0.048, 0.098, -0.002, 0.026, -0.052, 0.052, -0.056),
    (4.32, 0.058, 0.098, 0.208, -0.004, 0.052, -0.100, 0.104, -0.108),
    (3.92, 0.094, 0.150, 0.340, -0.006, 0.086, -0.150, 0.158, -0.160),
    (3.46, 0.140, 0.204, 0.492, -0.008, 0.128, -0.200, 0.214, -0.212),
    (2.96, 0.196, 0.256, 0.660, -0.008, 0.176, -0.248, 0.268, -0.260),
    (2.42, 0.258, 0.304, 0.842, -0.006, 0.228, -0.292, 0.318, -0.304),
    (1.86, 0.318, 0.344, 1.020, -0.004, 0.280, -0.330, 0.360, -0.342),
    (1.30, 0.372, 0.378, 1.182, -0.002, 0.330, -0.362, 0.394, -0.374),
    (0.72, 0.418, 0.404, 1.328, 0.000, 0.378, -0.390, 0.420, -0.402),
    (0.10, 0.456, 0.424, 1.462, -0.004, 0.422, -0.414, 0.440, -0.428),
    (-0.55, 0.490, 0.438, 1.572, -0.012, 0.462, -0.432, 0.454, -0.448),
    (-1.25, 0.518, 0.446, 1.658, -0.024, 0.496, -0.446, 0.462, -0.464),
    (-1.98, 0.540, 0.448, 1.708, -0.038, 0.522, -0.454, 0.464, -0.474),
    (-2.72, 0.554, 0.442, 1.720, -0.052, 0.540, -0.458, 0.458, -0.478),
    (-3.44, 0.562, 0.428, 1.688, -0.066, 0.550, -0.458, 0.444, -0.476),
    (-4.10, 0.560, 0.404, 1.590, -0.078, 0.548, -0.452, 0.420, -0.468),
    (-4.54, 0.510, 0.360, 1.360, -0.062, 0.500, -0.428, 0.372, -0.444),
    (-4.92, 0.420, 0.296, 1.070, -0.040, 0.412, -0.352, 0.302, -0.366),
]

FUSE = [fuse_ring(*row) for row in FUSE_TABLE]
FS = Surf(FUSE, 1.0)


def UX(x):
    """model x -> fuselage surface u."""
    xs = [r[0] for r in FUSE_TABLE]
    for i in range(len(xs) - 1):
        if xs[i] >= x >= xs[i + 1]:
            t = (xs[i] - x) / (xs[i] - xs[i + 1])
            return (i + t) / float(len(xs) - 1)
    return 0.0 if x > xs[0] else 1.0


def deck_y(x):
    return FS.pt(UX(x), 0.0)[1]


def build_fuselage():
    with shell("fuselage"):
        loft(FUSE, HULL)
        cap(FUSE[0], (1.0, 0.0, 0.0), HULL)
        cap(FUSE[-1], (-1.0, 0.0, 0.0), HULL)


# ---------------------------------------------------------------------------
# LIFTING SURFACES
#
# Every wing / tail is a diamond ("double wedge") aerofoil lofted between ribs:
# sharp leading edge point, upper crest, upper aft break, sharp trailing edge,
# and the two lower equivalents.  Six vertices per rib.
#   rib index -> v:  0 LE .000 | 1 upper crest .167 | 2 upper aft .333
#                    3 TE .500 | 4 lower aft .667  | 5 lower crest .833
# so v in (0, .5) is the upper surface and v in (.5, 1) the lower surface.
# ---------------------------------------------------------------------------

def wedge_rib(le_x, te_x, span_pos, mid_y, thick, axis="z", camber=0.55):
    c = le_x - te_x
    up, dn = thick * camber, thick * (1.0 - camber)
    xs = [le_x, le_x - 0.32 * c, le_x - 0.68 * c, te_x, le_x - 0.68 * c, le_x - 0.32 * c]
    ys = [mid_y, mid_y + up, mid_y + up * 0.52, mid_y + thick * 0.02,
          mid_y - dn * 0.46, mid_y - dn]
    if axis == "z":
        return [(xs[i], ys[i], span_pos) for i in range(6)]
    return [(xs[i], span_pos, ys[i]) for i in range(6)]


def build_wing():
    """Right wing.  Trapezoidal, 42 deg LE sweep, slightly forward-swept TE,
    square-cut tip, essentially zero dihedral."""
    root_z, tip_z = 1.28, 4.02
    root_le, root_te = 0.42, -3.30
    swp_le, swp_te = math.tan(math.radians(42.0)), math.tan(math.radians(10.0))
    rings = []
    for z in (1.28, 1.62, 2.00, 2.40, 2.80, 3.20, 3.62, 4.02):
        s = z - root_z
        t = s / (tip_z - root_z)
        thick = 0.255 * (1.0 - t) + 0.050 * t
        rings.append(wedge_rib(root_le - s * swp_le, root_te + s * swp_te,
                               z, -0.020 + 0.028 * s, thick, "z"))
    with shell("wing"):
        loft(rings, HULL, flip=True)          # rib order makes the raw winding inward
        cap(rings[0], (0.0, 0.0, -1.0), HULL)  # root, buried in the fuselage
        cap(rings[-1], (0.0, 0.0, 1.0), HULL)  # square tip
    S = Surf(rings, -1.0)
    # leading edge flap + flaperon/aileron hinge lines, both surfaces
    for v in (0.185, 0.315, 0.685, 0.815):
        line_u(S, 0.02, 0.98, v, 10, 0.040, ACCENT, 0.007)
    # rib / spar lines
    for u in (0.20, 0.40, 0.60, 0.78):
        line_v(S, u, 0.05, 0.45, 5, 0.034, ACCENT, 0.007)
        line_v(S, u, 0.55, 0.95, 5, 0.034, ACCENT, 0.007)
    plate(S, 0.06, 0.22, 0.20, 0.30, 0.012, HULL, 2, 2)
    plate(S, 0.28, 0.44, 0.20, 0.30, 0.012, HULL, 2, 2)
    plate(S, 0.10, 0.26, 0.70, 0.80, 0.010, HULL, 2, 2)
    plate(S, 0.46, 0.60, 0.70, 0.80, 0.010, HULL, 2, 2)
    plate(S, 0.66, 0.80, 0.22, 0.28, 0.012, ACCENT, 1, 1)   # wingtip RWR diamond


def build_stab():
    """Right horizontal stabilator, set aft of the wing with a clear gap."""
    root_z, tip_z = 0.96, 2.95
    root_le, root_te = -3.05, -5.22
    swp_le, swp_te = math.tan(math.radians(35.0)), math.tan(math.radians(7.0))
    rings = []
    for z in (0.96, 1.36, 1.76, 2.16, 2.56, 2.95):
        s = z - root_z
        t = s / (tip_z - root_z)
        thick = 0.170 * (1.0 - t) + 0.046 * t
        rings.append(wedge_rib(root_le - s * swp_le, root_te + s * swp_te,
                               z, -0.115 + 0.010 * s, thick, "z"))
    with shell("stabilator"):
        loft(rings, HULL, flip=True)
        cap(rings[0], (0.0, 0.0, -1.0), HULL)
        cap(rings[-1], (0.0, 0.0, 1.0), HULL)
    S = Surf(rings, -1.0)
    for v in (0.22, 0.78):
        line_u(S, 0.05, 0.98, v, 8, 0.036, ACCENT, 0.006)
    for u in (0.30, 0.58):
        line_v(S, u, 0.05, 0.45, 4, 0.032, ACCENT, 0.006)
        line_v(S, u, 0.55, 0.95, 4, 0.032, ACCENT, 0.006)
    plate(S, 0.10, 0.28, 0.22, 0.32, 0.010, HULL, 2, 2)
    plate(S, 0.10, 0.28, 0.68, 0.78, 0.010, HULL, 2, 2)


FIN_CANT = math.radians(37.0)   # splayed noticeably harder than the real 28 deg
FIN_BASE = (0.0, 0.398, 0.585)


def build_fin():
    """Right vertical stabiliser: built upright, then canted outward about +X.

    The lowest two ribs sit below the deck and are flared in thickness, which
    gives a root fairing for free.
    """
    tip_y = 1.88
    root_le, root_te = -1.32, -4.42
    swp_le, swp_te = math.tan(math.radians(44.0)), math.tan(math.radians(2.5))
    rings = []
    for y in (-0.24, -0.06, 0.16, 0.52, 0.92, 1.32, 1.64, 1.88):
        t = max(0.0, y) / tip_y
        le = root_le - max(0.0, y) * swp_le
        te = root_te + max(0.0, y) * swp_te
        thick = 0.148 * (1.0 - t) + 0.052 * t
        if y < 0.16:                       # root fairing flare
            thick += (0.16 - y) * 0.95
        rib = wedge_rib(le, te, y, 0.0, thick, "y")
        rings.append([vadd(rot_x(p, FIN_CANT), FIN_BASE) for p in rib])
    with shell("fin"):
        loft(rings, HULL)
        cap(rings[0], rot_x((0.0, -1.0, 0.0), FIN_CANT), HULL)
        cap(rings[-1], rot_x((0.0, 1.0, 0.0), FIN_CANT), HULL)   # truncated top
    S = Surf(rings, 1.0)
    for v in (0.20, 0.30, 0.70, 0.80):
        line_u(S, 0.28, 0.98, v, 8, 0.036, ACCENT, 0.006)
    for u in (0.42, 0.62, 0.80):
        line_v(S, u, 0.05, 0.45, 4, 0.032, ACCENT, 0.006)
        line_v(S, u, 0.55, 0.95, 4, 0.032, ACCENT, 0.006)
    plate(S, 0.30, 0.50, 0.22, 0.32, 0.012, HULL, 2, 2)
    plate(S, 0.30, 0.50, 0.68, 0.78, 0.012, HULL, 2, 2)
    plate(S, 0.82, 0.94, 0.23, 0.29, 0.012, ACCENT, 1, 1)   # fin tip RWR diamond
    plate(S, 0.82, 0.94, 0.71, 0.77, 0.012, ACCENT, 1, 1)


# ---------------------------------------------------------------------------
# INTAKE TRUNK + CARET APERTURE
# ---------------------------------------------------------------------------

NAC_Z = 1.20
NAC_FRONT = 1.94


def nac_ring(x, zc, hw, ytop, ybot, bev=0.075):
    """8 point rounded-rectangle trunk section, clockwise seen from the nose."""
    return [
        (x, ytop, zc - hw + bev), (x, ytop, zc + hw - bev),
        (x, ytop - bev, zc + hw), (x, ybot + bev, zc + hw),
        (x, ybot, zc + hw - bev), (x, ybot, zc - hw + bev),
        (x, ybot + bev, zc - hw), (x, ytop - bev, zc - hw),
    ]


def caret_rake(p):
    """Rake the aperture plane: outboard goes aft, lower goes forward.  That skew
    is what makes a caret intake read as a caret rather than as a plain hole."""
    x, y, z = p
    dz = (z - (NAC_Z - 0.46)) / 0.92
    dy = (y + 0.60) / 0.58
    return (x - 0.62 * dz - 0.34 * dy, y, z)


def build_intake():
    """Right intake trunk under the wing-root chine, with a caret aperture."""
    ap = [caret_rake(p) for p in nac_ring(NAC_FRONT, NAC_Z, 0.46, -0.020, -0.600)]
    rings = [
        ap,
        nac_ring(1.10, NAC_Z, 0.470, -0.030, -0.610),
        nac_ring(0.10, NAC_Z + 0.02, 0.470, -0.060, -0.618),
        nac_ring(-1.10, NAC_Z - 0.02, 0.455, -0.110, -0.610),
        nac_ring(-2.20, NAC_Z - 0.10, 0.420, -0.170, -0.590),
        nac_ring(-3.10, NAC_Z - 0.24, 0.360, -0.230, -0.560),
        nac_ring(-3.80, NAC_Z - 0.40, 0.290, -0.280, -0.520),
    ]
    loft(rings, HULL)
    cap(rings[-1], (-1.0, 0.0, 0.0), HULL)
    ac = centroid(ap)
    inner = [lerp(p, ac, 0.055) for p in ap]
    ducts = [inner]
    for (t, dx, dz) in ((0.20, -0.55, -0.05), (0.45, -1.30, -0.12), (0.72, -2.05, -0.18)):
        ducts.append([vadd(lerp(p, ac, t), (dx, 0.0, dz)) for p in ap])
    loft(ducts, ACCENT, flip=True)                    # interior: normals inward
    cap(ducts[-1], (1.0, 0.0, 0.0), ACCENT)           # seen looking into the mouth
    n = len(ap)
    for j in range(n):                                # sharp aperture lip
        k = (j + 1) % n
        quad_out(ap[j], ap[k], inner[k], inner[j], (1.0, 0.0, 0.0), ACCENT)
    S = Surf(rings, 1.0)
    # trunk chine lines
    for v in (0.130, 0.620):
        line_u(S, 0.03, 0.97, v, 9, 0.044, ACCENT, 0.007)
    for u in (0.22, 0.44, 0.66):
        line_v(S, u, 0.02, 0.48, 4, 0.038, ACCENT, 0.007)
    # main landing gear door, underside of the trunk (v ~0.56..0.68 is the floor)
    door(S, 0.24, 0.58, 0.575, 0.665, 7, 0.010, HULL)
    # side weapons bay door, outboard wall (v ~0.28..0.44)
    door(S, 0.16, 0.46, 0.300, 0.420, 6, 0.010, HULL)
    plate(S, 0.62, 0.78, 0.30, 0.42, 0.013, HULL, 2, 2)
    plate(S, 0.30, 0.42, 0.03, 0.11, 0.013, HULL, 2, 1)
    plate(S, 0.52, 0.64, 0.03, 0.11, 0.013, HULL, 2, 1)


# ---------------------------------------------------------------------------
# 2D THRUST-VECTORING NOZZLE
# ---------------------------------------------------------------------------

NOZ_Z = 0.585
SEG = 5


def noz_ring(x, hw, hh, yc, bev=0.075, seg=SEG):
    """Rectangular section, clockwise seen from the nose.  The top and bottom
    runs are subdivided so the exit ring can be turned into a saw-tooth."""
    top, bot = yc + hh, yc - hh
    pts = []
    for i in range(seg + 1):
        t = i / float(seg)
        pts.append((x, top, (NOZ_Z - hw + bev) + t * (2 * hw - 2 * bev)))
    pts.append((x, top - bev, NOZ_Z + hw))
    pts.append((x, bot + bev, NOZ_Z + hw))
    for i in range(seg + 1):
        t = i / float(seg)
        pts.append((x, bot, (NOZ_Z + hw - bev) - t * (2 * hw - 2 * bev)))
    pts.append((x, bot + bev, NOZ_Z - hw))
    pts.append((x, top - bev, NOZ_Z - hw))
    return pts


def build_nozzle():
    """Right nozzle: rectangular 2D vectoring nozzle with serrated exit flaps.

    This is what the player stares at from the chase camera, so it gets the most
    geometry per unit of surface area of anything on the aircraft.
    """
    rings = [
        noz_ring(-3.85, 0.300, 0.246, -0.072, 0.070),
        noz_ring(-4.40, 0.358, 0.254, -0.056, 0.072),
        noz_ring(-4.85, 0.418, 0.262, -0.040, 0.072),   # emerges from the tail here
        noz_ring(-5.14, 0.442, 0.254, -0.030, 0.066),
        noz_ring(-5.32, 0.428, 0.230, -0.026, 0.058),
    ]
    saw = []
    for i, p in enumerate(rings[-1]):
        flat_top = i <= SEG
        flat_bot = (SEG + 3) <= i <= (2 * SEG + 3)
        if flat_top or flat_bot:
            j = i if flat_top else i - (SEG + 3)
            back = -0.108 if (j % 2 == 0) else -0.006
        else:
            back = -0.040
        saw.append((p[0] + back, p[1], p[2]))
    rings.append(saw)
    loft(rings, HULL)
    sc = centroid(saw)
    inner = [lerp(p, sc, 0.075) for p in saw]
    i1 = [vadd(lerp(p, sc, 0.32), (0.44, 0.0, 0.0)) for p in saw]
    i2 = [vadd(lerp(p, sc, 0.55), (0.92, 0.0, 0.0)) for p in saw]
    n = len(saw)
    for j in range(n):                                 # aft-facing lip
        k = (j + 1) % n
        quad_out(saw[j], saw[k], inner[k], inner[j], (-1.0, 0.0, 0.0), ACCENT)
    loft([i2, i1, inner], ACCENT, flip=True)           # duct interior
    cap(i2, (-1.0, 0.0, 0.0), ACCENT)                  # seen from behind
    S = Surf(rings, 1.0)
    # convergent/divergent flap seams down the sides of the nozzle
    for v in (0.02, 0.10, 0.52, 0.60):
        line_u(S, 0.02, 0.98, v, 6, 0.034, ACCENT, 0.006)
    for u in (0.34, 0.58):
        line_v(S, u, 0.00, 0.42, 5, 0.028, ACCENT, 0.006)
        line_v(S, u, 0.50, 0.92, 5, 0.028, ACCENT, 0.006)
    # actuator fairings on the upper and lower flaps, kept clear of the exit
    plate(S, 0.26, 0.64, 0.18, 0.28, 0.022, HULL, 3, 1)
    plate(S, 0.26, 0.64, 0.68, 0.78, 0.022, HULL, 3, 1)
    plate(S, 0.26, 0.64, 0.38, 0.46, 0.018, HULL, 3, 1)
    plate(S, 0.26, 0.64, 0.88, 0.96, 0.018, HULL, 3, 1)


# ---------------------------------------------------------------------------
# CANOPY + DORSAL SPINE
# ---------------------------------------------------------------------------

def bubble(prof, arc, m, name, sill=False):
    """Loft a faceted half-dome along the spine from (x, half width, height)."""
    rings = []
    for (x, w, h) in prof:
        by = deck_y(x) - 0.028
        ring = []
        for i in range(arc + 1):
            a = math.pi * i / float(arc)
            # start on -Z, arc over the top, finish on +Z: clockwise from the nose
            ring.append((x, by + h * (math.sin(a) ** 0.72), -w * math.cos(a)))
        ring.append((x, by, 0.0))
        rings.append(ring)
    with shell(name, m):
        loft(rings, m)
        cap(rings[0], (1.0, 0.0, 0.0), m)
        cap(rings[-1], (-1.0, 0.0, 0.0), m)
    if sill:
        for i in range(len(prof) - 1):
            (x0, w0, _), (x1, w1, _) = prof[i], prof[i + 1]
            for s in (1.0, -1.0):
                quad_out((x0, deck_y(x0) - 0.026, s * (w0 + 0.004)),
                         (x1, deck_y(x1) - 0.026, s * (w1 + 0.004)),
                         (x1, deck_y(x1) + 0.008, s * (w1 + 0.060)),
                         (x0, deck_y(x0) + 0.008, s * (w0 + 0.060)),
                         (0.0, 1.0, 0.0), HULL)
    return Surf(rings, 1.0)


CANOPY_PROF = [
    (3.34, 0.020, 0.010), (3.02, 0.126, 0.122), (2.66, 0.208, 0.220),
    (2.24, 0.282, 0.300), (1.78, 0.330, 0.352), (1.30, 0.350, 0.366),
    (0.82, 0.348, 0.352), (0.32, 0.332, 0.312), (-0.22, 0.306, 0.252),
    (-0.80, 0.278, 0.192), (-1.30, 0.254, 0.148),
]

SPINE_PROF = [
    (-1.30, 0.254, 0.146), (-1.98, 0.240, 0.116), (-2.72, 0.224, 0.090),
    (-3.44, 0.204, 0.066), (-4.10, 0.182, 0.046), (-4.54, 0.158, 0.030),
    (-4.92, 0.132, 0.016),
]


def build_canopy():
    S = bubble(CANOPY_PROF, 8, ACCENT, "canopy", sill=True)
    # windscreen bow and canopy frame joint
    line_v(S, 0.205, 0.03, 0.97, 10, 0.045, HULL, 0.010)


def build_spine():
    S = bubble(SPINE_PROF, 6, HULL, "spine")
    plate(S, 0.06, 0.20, 0.42, 0.58, 0.010, ACCENT, 2, 2)   # refuelling receptacle
    for v in (0.16, 0.84):
        line_u(S, 0.05, 0.95, v, 8, 0.038, ACCENT, 0.007)


# ---------------------------------------------------------------------------
# SMALL DETAIL
# ---------------------------------------------------------------------------

def build_probe(base, direction, length, r0, r1, m=HULL, sides=5):
    d = vnorm(direction)
    up = (0.0, 1.0, 0.0) if abs(d[1]) < 0.9 else (1.0, 0.0, 0.0)
    e1 = vnorm(vcross(d, up))
    e2 = vcross(d, e1)
    rings = []
    for (t, r) in ((0.0, r0), (0.62, r1 * 1.25), (1.0, r1)):
        c = vadd(base, vmul(d, length * t))
        rings.append([vadd(c, vadd(vmul(e1, r * math.cos(2 * math.pi * i / sides)),
                                   vmul(e2, r * math.sin(2 * math.pi * i / sides))))
                      for i in range(sides)])
    mid = vadd(base, vmul(d, length * 0.5))
    for i in range(len(rings) - 1):
        A, B = rings[i], rings[i + 1]
        for j in range(sides):
            k = (j + 1) % sides
            quad_out(A[j], B[j], B[k], A[k], vnorm(vsub(A[j], mid)), m)
    cap(rings[-1], d, m)
    cap(rings[0], vmul(d, -1.0), m)


def build_details():
    # --- belly: main weapons bay doors, saw-tooth seamed ---------------------
    door(FS, UX(0.55), UX(-2.35), 0.442, 0.496, 11, 0.010, HULL)
    door(FS, UX(0.55), UX(-2.35), 0.504, 0.558, 11, 0.010, HULL)
    # nose gear door
    door(FS, UX(2.30), UX(1.15), 0.470, 0.530, 5, 0.010, HULL)
    # aft belly / engine bay access
    door(FS, UX(-2.60), UX(-4.20), 0.455, 0.545, 7, 0.010, HULL)

    # --- cheeks and chine ----------------------------------------------------
    for v in (0.130, 0.870):
        line_u(FS, UX(4.40), UX(-4.60), v, 14, 0.044, ACCENT, 0.008)
    for v in (0.295, 0.705):
        line_u(FS, UX(3.90), UX(-4.60), v, 12, 0.040, ACCENT, 0.008)
    for v in (0.208, 0.792):
        line_u(FS, UX(4.10), UX(-2.00), v, 10, 0.032, ACCENT, 0.008)
    # transverse frames on the cheeks
    for x in (3.40, 2.40, 1.30, 0.10, -1.25, -2.72, -4.10):
        line_v(FS, UX(x), 0.09, 0.42, 5, 0.030, ACCENT, 0.008)
        line_v(FS, UX(x), 0.58, 0.91, 5, 0.030, ACCENT, 0.008)

    # --- top deck ------------------------------------------------------------
    plate(FS, UX(4.10), UX(3.40), 0.965, 1.035, 0.012, HULL, 2, 2)
    plate(FS, UX(3.40), UX(2.70), 0.965, 1.035, 0.012, HULL, 2, 2)
    saw_v(FS, UX(2.62), UX(2.44), 0.955, 1.045, 5, ACCENT, 0.010)
    # aft deck between the fins
    plate(FS, UX(-2.90), UX(-3.60), 0.030, 0.075, 0.012, HULL, 2, 1)
    plate(FS, UX(-2.90), UX(-3.60), 0.925, 0.970, 0.012, HULL, 2, 1)
    plate(FS, UX(-3.70), UX(-4.40), 0.030, 0.075, 0.012, HULL, 2, 1)
    plate(FS, UX(-3.70), UX(-4.40), 0.925, 0.970, 0.012, HULL, 2, 1)
    saw_v(FS, UX(-4.46), UX(-4.28), 0.020, 0.085, 4, ACCENT, 0.010)
    saw_v(FS, UX(-4.46), UX(-4.28), 0.915, 0.980, 4, ACCENT, 0.010)

    # --- RWR / sensor diamonds ----------------------------------------------
    for (x, v) in ((3.70, 0.150), (3.70, 0.850), (2.60, 0.150), (2.60, 0.850),
                   (-3.10, 0.150), (-3.10, 0.850)):
        u = UX(x)
        plate(FS, u, u + 0.030, v - 0.020, v + 0.020, 0.013, ACCENT, 1, 1)
    # gun port: the pilot's right is model +Z, so v < 0.5
    u = UX(0.10)
    plate(FS, u - 0.012, u + 0.012, 0.118, 0.152, 0.012, ACCENT, 1, 1)

    # --- probes --------------------------------------------------------------
    for s in (1.0, -1.0):
        build_probe((3.70, -0.090, s * 0.30), (0.60, -0.05, s * 0.80), 0.26, 0.026, 0.011, HULL)
        build_probe((4.15, 0.045, s * 0.16), (0.30, 0.86, s * 0.40), 0.16, 0.020, 0.008, HULL)


# ---------------------------------------------------------------------------
# assemble
# ---------------------------------------------------------------------------

# Export transform.  See the SIZE section of the module docstring.
# p_out = (x * S + SHIFT_X, y * S, z * S)
EXPORT_SCALE = 0.649
EXPORT_SHIFT_X = 1.923      # puts the tail 1.60 behind the origin, as today


def finalize(scale, shift_x):
    """Apply the export transform. Uniform positive scale, so face winding and
    the closed-shell volume signs are unaffected."""
    for m in TRIS:
        TRIS[m] = [tuple((p[0] * scale + shift_x, p[1] * scale, p[2] * scale)
                         for p in t) for t in TRIS[m]]


def build():
    build_fuselage()
    build_canopy()
    build_spine()
    mirrored(build_wing)
    mirrored(build_stab)
    mirrored(build_fin)
    mirrored(build_intake)
    mirrored(build_nozzle)
    build_details()


# ---------------------------------------------------------------------------
# GLB writing
# ---------------------------------------------------------------------------

MATERIALS = [
    {
        "name": "Hull",
        "doubleSided": False,
        "pbrMetallicRoughness": {
            "baseColorFactor": [0.550, 0.566, 0.588, 1.0],
            "metallicFactor": 0.55,
            "roughnessFactor": 0.42,
        },
    },
    {
        "name": "Accent_Glass",
        "doubleSided": False,
        "pbrMetallicRoughness": {
            "baseColorFactor": [0.098, 0.110, 0.132, 1.0],
            "metallicFactor": 0.80,
            "roughnessFactor": 0.17,
        },
    },
]


def luminance(c):
    return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]


def build_arrays(tri_list):
    """Flat shading: one normal per face; vertices merged only when position and
    normal both match."""
    seen, pos, nrm, idx = {}, [], [], []
    for (a, b, c) in tri_list:
        n = vnorm(vcross(vsub(b, a), vsub(c, a)))
        nk = (round(n[0], 4), round(n[1], 4), round(n[2], 4))
        for p in (a, b, c):
            key = (round(p[0], 5), round(p[1], 5), round(p[2], 5)) + nk
            i = seen.get(key)
            if i is None:
                i = len(pos)
                seen[key] = i
                pos.append(p)
                nrm.append(n)
            idx.append(i)
    return pos, nrm, idx


def pad4(b, fill=b"\x00"):
    while len(b) % 4:
        b += fill
    return b


def write_glb(path):
    buf = bytearray()
    bufviews, accessors, meshes, nodes = [], [], [], []

    def add_view(data, target):
        while len(buf) % 4:
            buf.append(0)
        off = len(buf)
        buf.extend(data)
        bufviews.append({"buffer": 0, "byteOffset": off,
                         "byteLength": len(data), "target": target})
        return len(bufviews) - 1

    for mat_idx, name in ((HULL, "Hull"), (ACCENT, "Accent_Glass")):
        pos, nrm, idx = build_arrays(TRIS[mat_idx])
        pdata = b"".join(struct.pack("<3f", *p) for p in pos)
        ndata = b"".join(struct.pack("<3f", *n) for n in nrm)
        if len(pos) < 65536:
            idata, ctype = b"".join(struct.pack("<H", i) for i in idx), 5123
        else:
            idata, ctype = b"".join(struct.pack("<I", i) for i in idx), 5125
        pv, nv = add_view(pdata, 34962), add_view(ndata, 34962)
        iv = add_view(pad4(idata), 34963)
        accessors.append({"bufferView": pv, "componentType": 5126, "count": len(pos),
                          "type": "VEC3",
                          "min": [min(p[k] for p in pos) for k in range(3)],
                          "max": [max(p[k] for p in pos) for k in range(3)]})
        accessors.append({"bufferView": nv, "componentType": 5126, "count": len(nrm),
                          "type": "VEC3"})
        accessors.append({"bufferView": iv, "componentType": ctype, "count": len(idx),
                          "type": "SCALAR"})
        base = len(accessors) - 3
        meshes.append({"name": name, "primitives": [{
            "attributes": {"POSITION": base, "NORMAL": base + 1},
            "indices": base + 2, "material": mat_idx, "mode": 4}]})
        nodes.append({"name": name, "mesh": len(meshes) - 1})

    gltf = {
        "asset": {"version": "2.0", "generator": "genjet.py (stdlib) - F-22 style"},
        "scene": 0,
        "scenes": [{"name": "Jet", "nodes": list(range(len(nodes)))}],
        "nodes": nodes,
        "meshes": meshes,
        "materials": MATERIALS,
        "accessors": accessors,
        "bufferViews": bufviews,
        "buffers": [{"byteLength": len(buf)}],
    }
    jchunk = pad4(json.dumps(gltf, separators=(",", ":")).encode("utf-8"), b" ")
    bchunk = pad4(bytes(buf))
    total = 12 + 8 + len(jchunk) + 8 + len(bchunk)
    with open(path, "wb") as f:
        f.write(struct.pack("<III", 0x46546C67, 2, total))
        f.write(struct.pack("<II", len(jchunk), 0x4E4F534A))
        f.write(jchunk)
        f.write(struct.pack("<II", len(bchunk), 0x004E4942))
        f.write(bchunk)
    return total


# ---------------------------------------------------------------------------
# verification
# ---------------------------------------------------------------------------

def report():
    total = len(TRIS[HULL]) + len(TRIS[ACCENT])
    allp = [p for m in TRIS for t in TRIS[m] for p in t]
    mn = [min(p[k] for p in allp) for k in range(3)]
    mx = [max(p[k] for p in allp) for k in range(3)]
    print("triangles      : %d  (hull %d, accent %d)"
          % (total, len(TRIS[HULL]), len(TRIS[ACCENT])))
    print("degenerate     : %d rejected at build time" % DEGENERATE[0])
    print("model bbox     : min %s  max %s"
          % ([round(v, 3) for v in mn], [round(v, 3) for v in mx]))
    print("model size     : x %.2f  y %.2f  z %.2f"
          % (mx[0] - mn[0], mx[1] - mn[1], mx[2] - mn[2]))
    print("ship space     : length(z) %.2f  span(x) %.2f  height(y) %.2f   (ratio %.2f:1)"
          % (mx[0] - mn[0], mx[2] - mn[2], mx[1] - mn[1],
             (mx[0] - mn[0]) / max(mx[2] - mn[2], 1e-6)))
    print("               : ship z %.2f .. %.2f   (spaceship.glb: -1.38 .. 4.24)"
          % (mn[0], mx[0]))
    for mat in MATERIALS:
        c = mat["pbrMetallicRoughness"]["baseColorFactor"]
        lum = luminance(c)
        print("material %-13s base=%s luminance=%.4f -> %s"
              % (mat["name"], [round(v, 3) for v in c[:3]], lum,
                 "ACCENT (< 0.35)" if lum < 0.35 else "HULL (>= 0.35)"))
    bad = []
    for (name, m, s, e) in SHELLS:
        vol = sum(vdot(a, vcross(b, c)) / 6.0 for (a, b, c) in TRIS[m][s:e])
        if vol <= 0:
            bad.append((name, round(vol, 4)))
    if bad:
        print("!! INWARD-FACING closed shells: %s" % bad)
    else:
        print("normals        : all %d closed shells have positive signed volume"
              % len(SHELLS))
    z = sum(1 for m in TRIS for (a, b, c) in TRIS[m]
            if vlen(vcross(vsub(b, a), vsub(c, a))) < 1e-9)
    print("zero-area faces: %d" % z)


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    root = os.path.dirname(os.path.dirname(here))
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    brief = "--brief-size" in sys.argv
    out = args[0] if args else os.path.join(root, "public", "jet.glb")
    build()
    finalize(1.0 if brief else EXPORT_SCALE, 0.0 if brief else EXPORT_SHIFT_X)
    report()
    print("wrote %s (%.1f KB)" % (out, write_glb(out) / 1024.0))


if __name__ == "__main__":
    main()
