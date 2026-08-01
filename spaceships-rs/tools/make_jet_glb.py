#!/usr/bin/env python3
"""Turn the Sketchfab "Modern Jet Fighter Low Poly Game Ready Free" download into
`public/jet.glb`, a game-ready ship model for the spaceships client.

Source model
    "Modern Jet Fighter Low Poly Game Ready Free" by Hdjusj, CC-BY-4.0.
    https://sketchfab.com/3d-models/modern-jet-fighter-low-poly-game-ready-free-cd2bd715dcd14dc4b47ebaeb2403fb89
    Attribution is mandatory; the credit is written into the GLB's asset.extras
    and lives in README.md.

What it does
    1. Bakes the glTF node transform chain into one flat vertex list.
    2. Finds the landing gear as connected components hanging below the
       airframe and deletes those faces (the download has the gear DOWN, and
       everything is one mesh so there is no node to remove).
    3. Samples the source baseColor bake once, per face, at barycentric points
       inside the triangle, to learn which faces are dark (canopy glass,
       intake/exhaust cavities, gun port) and which are light (airframe skin).
       The bake itself is a photogrammetry-style auto-bake -- smeared islands,
       bleeding edges, grey mush -- so it is used ONLY as a classifier and is
       then thrown away. The output has no texture at all.
    4. Emits exactly two flat PBR materials: `hull` (luminance > 0.35) and
       `accent` (luminance < 0.35), which is what public/src/customization.js
       and public/src/ship.js split on.
    5. Orients nose to +X (public/src/ship.js applies rotation.y = -PI/2, which
       maps model +X to world +Z = flight-forward), recentres on the origin and
       scales to the envelope of the ship it replaces.

Dependencies: Python standard library only, plus macOS `sips` (used once to
decode the source JPEG into a PNG that stdlib zlib can read). Pass
--no-texture-classify to skip the texture step entirely.

Usage
    python3 spaceships-rs/tools/make_jet_glb.py \
        --src ~/Downloads/modern_jet_fighter_low_poly_game_ready_free \
        --out public/jet.glb
"""

import argparse
import json
import math
import os
import struct
import subprocess
import tempfile
import zlib
from collections import defaultdict

# ---------------------------------------------------------------------------
# Tunables. The defaults are what shipped; they are here so the conversion can
# be re-run and nudged without going hunting through the code.
# ---------------------------------------------------------------------------

# public/spaceship.glb measured with Box3.setFromObject (NOT raw accessor
# bounds -- its nodes carry scales of 0.11..0.66 and the raw bounds are ~1.5x
# the truth). X is the model-space fuselage axis for both models.
REF_LENGTH_X = 5.6187
REF_HEIGHT_Y = 1.7370
REF_WIDTH_Z = 5.4272

# Flat PBR values, chosen for the client's HDR path (ACES tonemap, bloom
# threshold 0.92 -- see public/src/graphics.js), and chosen so the luminance
# split in customization.js/ship.js lands on the right side of 0.35.
# baseColorFactor in glTF is LINEAR, and three.js exposes material.color in
# linear working space, so these numbers are exactly what isAccentMesh() sees.
HULL_SRGB = (0xA9, 0xB4, 0xC2)      # light airframe grey, faint blue cast
HULL_METALLIC = 0.20                # low: the standard (non-Ultra) scene has
HULL_ROUGHNESS = 0.45               # no environment map, and metal without one
ACCENT_SRGB = (0x23, 0x2A, 0x36)    # goes flat black
ACCENT_METALLIC = 0.45
ACCENT_ROUGHNESS = 0.30

# Nose-gear well: the lengthwise station to search, as (from_nose, to_nose)
# fractions of the aircraft's length, and the height below which anything found
# there is gear structure, as a fraction of its height above the lowest point.
NOSE_WELL_STATION = (0.20, 0.38)
NOSE_WELL_FLOOR = 0.072

# Face classification from the source bake.
TEX_MAX_SIDE = 1024        # the bake is 4096^2 of noise; 1024 is plenty
SMOOTH_ITERS = 6           # diffusion passes over the face-adjacency graph
DARK_PERCENTILE = 14.0     # darkest N% of airframe faces seed the accent set
MIN_ACCENT_PATCH = 12      # drop accent islands smaller than this (bake noise)
MASK_SMOOTH_ROUNDS = 3     # majority votes that straighten the accent border

# Barycentric sample points: centroid plus interior points, median-filtered.
BARY = [
    (1 / 3, 1 / 3, 1 / 3),
    (0.60, 0.20, 0.20), (0.20, 0.60, 0.20), (0.20, 0.20, 0.60),
    (0.45, 0.45, 0.10), (0.10, 0.45, 0.45), (0.45, 0.10, 0.45),
]

CREDIT = ('This work is based on "Modern Jet Fighter Low Poly Game Ready Free" '
          '(https://sketchfab.com/3d-models/modern-jet-fighter-low-poly-game-ready-free'
          '-cd2bd715dcd14dc4b47ebaeb2403fb89) by Hdjusj '
          '(https://sketchfab.com/Hdjusj) licensed under CC-BY-4.0 '
          '(http://creativecommons.org/licenses/by/4.0/)')


# ---------------------------------------------------------------------------
# glTF reading
# ---------------------------------------------------------------------------

COMPONENT = {5120: ('b', 1), 5121: ('B', 1), 5122: ('h', 2),
             5123: ('H', 2), 5125: ('I', 4), 5126: ('f', 4)}
NCOMP = {'SCALAR': 1, 'VEC2': 2, 'VEC3': 3, 'VEC4': 4, 'MAT4': 16}


def read_accessor(gltf, blob, index):
    a = gltf['accessors'][index]
    bv = gltf['bufferViews'][a['bufferView']]
    off = bv.get('byteOffset', 0) + a.get('byteOffset', 0)
    n = NCOMP[a['type']]
    fmt, size = COMPONENT[a['componentType']]
    stride = bv.get('byteStride') or n * size
    out = []
    for k in range(a['count']):
        out.append(struct.unpack_from('<' + fmt * n, blob, off + k * stride))
    return out


def mat_mul(a, b):
    """Column-major 4x4 multiply, a then b applied as (a*b)."""
    out = [0.0] * 16
    for c in range(4):
        for r in range(4):
            out[c * 4 + r] = sum(a[k * 4 + r] * b[c * 4 + k] for k in range(4))
    return out


def node_matrix(node):
    if 'matrix' in node:
        return list(node['matrix'])
    m = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]
    if 'rotation' in node:
        x, y, z, w = node['rotation']
        m = [1 - 2 * (y * y + z * z), 2 * (x * y + z * w), 2 * (x * z - y * w), 0,
             2 * (x * y - z * w), 1 - 2 * (x * x + z * z), 2 * (y * z + x * w), 0,
             2 * (x * z + y * w), 2 * (y * z - x * w), 1 - 2 * (x * x + y * y), 0,
             0, 0, 0, 1]
    if 'scale' in node:
        sx, sy, sz = node['scale']
        for i in range(4):
            m[i] *= sx
            m[4 + i] *= sy
            m[8 + i] *= sz
    if 'translation' in node:
        m[12], m[13], m[14] = node['translation']
    return m


def collect_mesh(gltf, blob):
    """Walk the scene graph, return baked (positions, normals, uvs, triangles)."""
    pos, nor, uv, tris = [], [], [], []
    ident = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]

    def visit(ni, parent):
        node = gltf['nodes'][ni]
        m = mat_mul(parent, node_matrix(node))
        if 'mesh' in node:
            for prim in gltf['meshes'][node['mesh']]['primitives']:
                if prim.get('mode', 4) != 4:
                    continue
                base = len(pos)
                p = read_accessor(gltf, blob, prim['attributes']['POSITION'])
                n = (read_accessor(gltf, blob, prim['attributes']['NORMAL'])
                     if 'NORMAL' in prim['attributes'] else [(0.0, 1.0, 0.0)] * len(p))
                t = (read_accessor(gltf, blob, prim['attributes']['TEXCOORD_0'])
                     if 'TEXCOORD_0' in prim['attributes'] else [(0.0, 0.0)] * len(p))
                for (x, y, z), (nx, ny, nz), st in zip(p, n, t):
                    pos.append((m[0] * x + m[4] * y + m[8] * z + m[12],
                                m[1] * x + m[5] * y + m[9] * z + m[13],
                                m[2] * x + m[6] * y + m[10] * z + m[14]))
                    nor.append((m[0] * nx + m[4] * ny + m[8] * nz,
                                m[1] * nx + m[5] * ny + m[9] * nz,
                                m[2] * nx + m[6] * ny + m[10] * nz))
                    uv.append(tuple(st))
                idx = ([v[0] for v in read_accessor(gltf, blob, prim['indices'])]
                       if 'indices' in prim else list(range(len(p))))
                for i in range(0, len(idx) - 2, 3):
                    tris.append((base + idx[i], base + idx[i + 1], base + idx[i + 2]))
        for ch in node.get('children', ()):
            visit(ch, m)

    for ni in gltf['scenes'][gltf.get('scene', 0)]['nodes']:
        visit(ni, ident)
    return pos, nor, uv, tris


# ---------------------------------------------------------------------------
# Topology
# ---------------------------------------------------------------------------

class DSU:
    def __init__(self, n):
        self.p = list(range(n))

    def find(self, a):
        while self.p[a] != a:
            self.p[a] = self.p[self.p[a]]
            a = self.p[a]
        return a

    def union(self, a, b):
        ra, rb = self.find(a), self.find(b)
        if ra != rb:
            self.p[ra] = rb


def weld(pos, quantum=1e-6):
    """Map duplicated vertices (UV seams split them) onto shared slots."""
    q = 1.0 / quantum
    table, out = {}, [0] * len(pos)
    for i, p in enumerate(pos):
        k = (round(p[0] * q), round(p[1] * q), round(p[2] * q))
        s = table.get(k)
        if s is None:
            s = table[k] = len(table)
        out[i] = s
    return out, len(table)


def components(tris, wmap, nweld):
    dsu = DSU(nweld)
    for a, b, c in tris:
        dsu.union(wmap[a], wmap[b])
        dsu.union(wmap[b], wmap[c])
    groups = defaultdict(list)
    for ti, (a, _b, _c) in enumerate(tris):
        groups[dsu.find(wmap[a])].append(ti)
    return sorted(groups.values(), key=len, reverse=True)


def face_adjacency(tris, wmap, keep=None):
    """Faces are neighbours when they share a welded edge."""
    edges = defaultdict(list)
    subset = range(len(tris)) if keep is None else keep
    for ti in subset:
        a, b, c = wmap[tris[ti][0]], wmap[tris[ti][1]], wmap[tris[ti][2]]
        for e in ((a, b), (b, c), (c, a)):
            edges[(e[0], e[1]) if e[0] < e[1] else (e[1], e[0])].append(ti)
    adj = defaultdict(set)
    for fl in edges.values():
        for i in range(len(fl)):
            for j in range(i + 1, len(fl)):
                adj[fl[i]].add(fl[j])
                adj[fl[j]].add(fl[i])
    return adj, edges


def face_centroid(tris, pos, ti):
    a, b, c = (pos[v] for v in tris[ti])
    return ((a[0] + b[0] + c[0]) / 3.0, (a[1] + b[1] + c[1]) / 3.0,
            (a[2] + b[2] + c[2]) / 3.0)


def find_gear_well(tris, pos, faces, wmap, station=NOSE_WELL_STATION,
                   floor=NOSE_WELL_FLOOR, min_group=20):
    """The nose-gear well and drag brace, which are welded into the belly.

    The three bogies were separate shells and are gone with them, but the nose
    well is part of the airframe and stays behind as a black boss hanging under
    the chin. This is a deliberately narrow, hand-identified cut rather than a
    clever general rule: a generic "anything below the belly line" test needs a
    smoothed belly profile, and the belly climbs steadily toward the tail, so
    every smoothing window wide enough to ignore a narrow boss also lags that
    climb and condemns large patches of good rear underside. Measured instead:
    the well is the only structure on the whole airframe below 7.2% of the
    aircraft's height, between 20% and 38% of its length aft of the nose. The
    next-lowest structure sits 0.008 (0.8% of length) higher, so the cut has
    real clearance. Bounds are fractions of the airframe box, so a re-export at
    a different scale still lands in the same place.
    """
    lo, hi = bbox(tris, pos, faces)
    length, height = hi[0] - lo[0], hi[1] - lo[1]
    x_lo = hi[0] - station[1] * length
    x_hi = hi[0] - station[0] * length
    y_cut = lo[1] + floor * height
    cent = {ti: face_centroid(tris, pos, ti) for ti in faces}
    cand = {ti for ti in faces
            if x_lo <= cent[ti][0] <= x_hi and cent[ti][1] < y_cut}
    print('  nose-well cut: x %.4f..%.4f, below y %.4f -> %d candidate faces'
          % (x_lo, x_hi, y_cut, len(cand)))
    adj, _ = face_adjacency(tris, wmap, cand)
    seen, out = set(), []
    for s in cand:
        if s in seen:
            continue
        stack, grp = [s], []
        while stack:
            v = stack.pop()
            if v in seen:
                continue
            seen.add(v)
            grp.append(v)
            for w in adj.get(v, ()):
                if w in cand and w not in seen:
                    stack.append(w)
        glo, ghi = bbox(tris, pos, grp)
        verdict = 'removed' if len(grp) >= min_group else 'too small, kept'
        print('    cluster %4d tris  x[%7.4f %7.4f] y[%7.4f %7.4f] z[%7.4f %7.4f]  %s'
              % (len(grp), glo[0], ghi[0], glo[1], ghi[1], glo[2], ghi[2], verdict))
        if len(grp) >= min_group:
            out.extend(grp)
    return out


def _boundary(tris, wmap, faces):
    ec = defaultdict(int)
    for ti in faces:
        a, b, c = wmap[tris[ti][0]], wmap[tris[ti][1]], wmap[tris[ti][2]]
        for e in ((a, b), (b, c), (c, a)):
            ec[(e[0], e[1]) if e[0] < e[1] else (e[1], e[0])] += 1
    return {e for e, n in ec.items() if n == 1}


def cap_openings(tris, pos, wmap, faces_before, faces_after):
    """Seal every opening that the deletion just created.

    Each connected group of fresh boundary edges gets a cone: one triangle per
    boundary edge, apex at the group's centroid. Winding comes from the
    surviving face on the far side of each edge, so the lid always faces the
    same way as the skin it closes -- which also copes with ragged fringes that
    are not simple loops.
    """
    fresh = _boundary(tris, wmap, faces_after) - _boundary(tris, wmap, faces_before)
    if not fresh:
        return []
    wpos = {}
    for i, p in enumerate(pos):
        wpos[wmap[i]] = p
    # directed edges of the surviving skin, so the lid can oppose them
    directed = {}
    for ti in faces_after:
        a, b, c = wmap[tris[ti][0]], wmap[tris[ti][1]], wmap[tris[ti][2]]
        for e in ((a, b), (b, c), (c, a)):
            directed[e] = ti
    dsu_index = {}
    verts = set()
    for a, b in fresh:
        verts.add(a)
        verts.add(b)
    for v in verts:
        dsu_index[v] = len(dsu_index)
    dsu = DSU(len(dsu_index))
    for a, b in fresh:
        dsu.union(dsu_index[a], dsu_index[b])
    groups = defaultdict(list)
    for e in fresh:
        groups[dsu.find(dsu_index[e[0]])].append(e)

    out = []
    for gid, edges in groups.items():
        gverts = {v for e in edges for v in e}
        cx = sum(wpos[v][0] for v in gverts) / len(gverts)
        cy = sum(wpos[v][1] for v in gverts) / len(gverts)
        cz = sum(wpos[v][2] for v in gverts) / len(gverts)
        apex = (cx, cy, cz)
        made = 0
        fan = []
        for a, b in edges:
            # the skin uses this edge in one direction; the lid uses the other
            if (a, b) in directed:
                p1, p2 = wpos[b], wpos[a]
            elif (b, a) in directed:
                p1, p2 = wpos[a], wpos[b]
            else:
                continue
            ux, uy, uz = p1[0] - cx, p1[1] - cy, p1[2] - cz
            vx, vy, vz = p2[0] - cx, p2[1] - cy, p2[2] - cz
            nx, ny, nz = uy * vz - uz * vy, uz * vx - ux * vz, ux * vy - uy * vx
            ln = math.sqrt(nx * nx + ny * ny + nz * nz)
            if ln < 1e-14:
                continue
            fan.append((apex, p1, p2, (nx, ny, nz)))
            made += 1
        # One shared normal for the whole lid. Per-facet normals on a cone over
        # a non-planar loop shade as a bright pinwheel; a single averaged normal
        # makes it read as one flat panel, which is what a closed bay looks like.
        ax = sum(t[3][0] for t in fan)
        ay = sum(t[3][1] for t in fan)
        az = sum(t[3][2] for t in fan)
        al = math.sqrt(ax * ax + ay * ay + az * az) or 1.0
        shared = (ax / al, ay / al, az / al)
        out.extend((t[0], t[1], t[2], shared) for t in fan)
        print('  sealed a %d-edge opening at (%.4f, %.4f, %.4f) with %d triangles'
              % (len(edges), cx, cy, cz, made))
    return out


def mirror_pairs(tris, pos, faces, tol=0.004):
    """Match each face with its twin reflected through the aircraft's z plane.

    The download is only *approximately* symmetric (mirrored parts disagree by
    ~2e-4), so this is a tolerance search over a spatial hash, not an exact
    lookup.
    """
    def centroid(ti):
        a, b, c = (pos[v] for v in tris[ti])
        return ((a[0] + b[0] + c[0]) / 3.0, (a[1] + b[1] + c[1]) / 3.0,
                (a[2] + b[2] + c[2]) / 3.0)

    cents = {ti: centroid(ti) for ti in faces}
    zs = [c[2] for c in cents.values()]
    plane = 0.5 * (min(zs) + max(zs))
    cell = tol
    grid = defaultdict(list)
    for ti, c in cents.items():
        grid[(int(math.floor(c[0] / cell)), int(math.floor(c[1] / cell)),
              int(math.floor(c[2] / cell)))].append(ti)
    out = {}
    for ti, c in cents.items():
        target = (c[0], c[1], 2.0 * plane - c[2])
        gi = (int(math.floor(target[0] / cell)), int(math.floor(target[1] / cell)),
              int(math.floor(target[2] / cell)))
        best, bestd = None, tol * tol
        for dx in (-1, 0, 1):
            for dy in (-1, 0, 1):
                for dz in (-1, 0, 1):
                    for tj in grid.get((gi[0] + dx, gi[1] + dy, gi[2] + dz), ()):
                        o = cents[tj]
                        d = ((o[0] - target[0]) ** 2 + (o[1] - target[1]) ** 2
                             + (o[2] - target[2]) ** 2)
                        if d < bestd:
                            best, bestd = tj, d
        if best is not None:
            out[ti] = best
    return out


def bbox(tris, pos, faces):
    lo = [1e30] * 3
    hi = [-1e30] * 3
    for ti in faces:
        for v in tris[ti]:
            for k in range(3):
                lo[k] = min(lo[k], pos[v][k])
                hi[k] = max(hi[k], pos[v][k])
    return lo, hi


# ---------------------------------------------------------------------------
# Texture (stdlib PNG reader; sips does the JPEG decode)
# ---------------------------------------------------------------------------

def read_png(path):
    data = open(path, 'rb').read()
    assert data[:8] == b'\x89PNG\r\n\x1a\n', 'not a PNG: %s' % path
    p, idat = 8, bytearray()
    w = h = ctype = None
    while p < len(data):
        ln, typ = struct.unpack_from('>I4s', data, p)
        p += 8
        chunk = data[p:p + ln]
        p += ln + 4
        if typ == b'IHDR':
            w, h, depth, ctype, _c, _f, interlace = struct.unpack('>IIBBBBB', chunk)
            assert depth == 8 and interlace == 0, 'unsupported PNG (%d bit, il=%d)' % (depth, interlace)
        elif typ == b'IDAT':
            idat += chunk
        elif typ == b'IEND':
            break
    nch = {0: 1, 2: 3, 4: 2, 6: 4}[ctype]
    raw = zlib.decompress(bytes(idat))
    stride = w * nch
    out = bytearray(stride * h)
    prev = bytearray(stride)
    pos = 0
    for y in range(h):
        f = raw[pos]
        pos += 1
        line = bytearray(raw[pos:pos + stride])
        pos += stride
        if f == 1:
            for i in range(nch, stride):
                line[i] = (line[i] + line[i - nch]) & 255
        elif f == 2:
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 255
        elif f == 3:
            for i in range(stride):
                a = line[i - nch] if i >= nch else 0
                line[i] = (line[i] + ((a + prev[i]) >> 1)) & 255
        elif f == 4:
            for i in range(stride):
                a = line[i - nch] if i >= nch else 0
                b = prev[i]
                c = prev[i - nch] if i >= nch else 0
                pp = a + b - c
                pa, pb, pc = abs(pp - a), abs(pp - b), abs(pp - c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[i] = (line[i] + pr) & 255
        elif f != 0:
            raise ValueError('unknown PNG filter %d' % f)
        out[y * stride:(y + 1) * stride] = line
        prev = line
    return w, h, nch, bytes(out)


def srgb_to_linear(c8):
    c = c8 / 255.0
    return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4


LIN = [srgb_to_linear(i) for i in range(256)]


def decode_basecolor(src, cache_dir):
    """sips -> PNG -> stdlib decode. One shot; the pixels are then discarded."""
    jpg = os.path.join(src, 'textures', 'material_0_baseColor.jpeg')
    if not os.path.exists(jpg):
        alt = [f for f in os.listdir(os.path.join(src, 'textures'))] if os.path.isdir(os.path.join(src, 'textures')) else []
        raise SystemExit('baseColor texture not found; textures/ holds %r' % alt)
    png = os.path.join(cache_dir, 'basecolor_%d.png' % TEX_MAX_SIDE)
    if not os.path.exists(png):
        subprocess.run(['sips', '-s', 'format', 'png', '-Z', str(TEX_MAX_SIDE), jpg,
                        '--out', png], check=True, capture_output=True)
    return read_png(png)


def face_luminance(tris, uv, tex):
    w, h, nch, pix = tex
    out = [0.0] * len(tris)
    for ti, t in enumerate(tris):
        s = []
        for b0, b1, b2 in BARY:
            u = uv[t[0]][0] * b0 + uv[t[1]][0] * b1 + uv[t[2]][0] * b2
            v = uv[t[0]][1] * b0 + uv[t[1]][1] * b1 + uv[t[2]][1] * b2
            # glTF UV origin is the image's TOP-left, and GLTFLoader sets
            # texture.flipY = false to preserve that, so row = v * height with
            # no flip. (Verified against three.js by texturing the source model
            # with a four-quadrant marker image and comparing wing colours.)
            x = int(u * w) % w
            y = int(v * h) % h
            o = (y * w + x) * nch
            s.append(0.2126 * LIN[pix[o]] + 0.7152 * LIN[pix[o + 1]] + 0.0722 * LIN[pix[o + 2]])
        s.sort()
        out[ti] = s[len(s) // 2]
    return out


# ---------------------------------------------------------------------------
# GLB writing
# ---------------------------------------------------------------------------

def pad4(b, fill=b'\x00'):
    r = (-len(b)) % 4
    return b + fill * r


def write_glb(path, groups, materials, extras, root_name='F22'):
    """groups: [(name, material_index, [(pos, nor)...], [tri indices...])]"""
    bin_parts, views, accessors, meshes, nodes = [], [], [], [], []
    offset = 0

    def add_view(payload, target):
        nonlocal offset
        payload = pad4(payload)
        views.append({'buffer': 0, 'byteOffset': offset,
                      'byteLength': len(payload), 'target': target})
        bin_parts.append(payload)
        offset += len(payload)
        return len(views) - 1

    for name, mat, verts, faces in groups:
        n = len(verts)
        big = n > 65535
        ifmt, ictype = ('I', 5125) if big else ('H', 5123)
        idx = bytearray()
        for tri in faces:
            idx += struct.pack('<' + ifmt * 3, *tri)
        iv = add_view(bytes(idx), 34963)
        accessors.append({'bufferView': iv, 'componentType': ictype, 'count': len(faces) * 3,
                          'type': 'SCALAR'})
        ai = len(accessors) - 1

        pbuf = bytearray()
        nbuf = bytearray()
        lo = [1e30] * 3
        hi = [-1e30] * 3
        for (p, nrm) in verts:
            pbuf += struct.pack('<3f', *p)
            nbuf += struct.pack('<3f', *nrm)
            for k in range(3):
                lo[k] = min(lo[k], p[k])
                hi[k] = max(hi[k], p[k])
        pv = add_view(bytes(pbuf), 34962)
        accessors.append({'bufferView': pv, 'componentType': 5126, 'count': n,
                          'type': 'VEC3', 'min': lo, 'max': hi})
        api = len(accessors) - 1
        nv = add_view(bytes(nbuf), 34962)
        accessors.append({'bufferView': nv, 'componentType': 5126, 'count': n, 'type': 'VEC3'})
        ani = len(accessors) - 1

        meshes.append({'name': name, 'primitives': [
            {'attributes': {'POSITION': api, 'NORMAL': ani}, 'indices': ai,
             'material': mat, 'mode': 4}]})
        nodes.append({'name': name, 'mesh': len(meshes) - 1})

    root = {'name': root_name, 'children': list(range(len(nodes)))}
    nodes.append(root)

    binchunk = pad4(b''.join(bin_parts))
    gltf = {
        'asset': {'version': '2.0', 'generator': 'spaceships-rs/tools/make_jet_glb.py',
                  'extras': extras},
        'scene': 0,
        'scenes': [{'name': 'Scene', 'nodes': [len(nodes) - 1]}],
        'nodes': nodes,
        'meshes': meshes,
        'materials': materials,
        'accessors': accessors,
        'bufferViews': views,
        'buffers': [{'byteLength': len(binchunk)}],
    }
    jsonchunk = pad4(json.dumps(gltf, separators=(',', ':')).encode('utf-8'), b' ')
    total = 12 + 8 + len(jsonchunk) + 8 + len(binchunk)
    with open(path, 'wb') as f:
        f.write(struct.pack('<III', 0x46546C67, 2, total))
        f.write(struct.pack('<II', len(jsonchunk), 0x4E4F534A))
        f.write(jsonchunk)
        f.write(struct.pack('<II', len(binchunk), 0x004E4942))
        f.write(binchunk)
    return total


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--src', default=os.path.expanduser(
        '~/Downloads/modern_jet_fighter_low_poly_game_ready_free'))
    ap.add_argument('--out', default='public/jet.glb')
    ap.add_argument('--keep-gear', action='store_true',
                    help='skip landing-gear removal (for before/after renders)')
    ap.add_argument('--no-texture-classify', action='store_true',
                    help='skip the baseColor sampling; split hull/accent geometrically')
    ap.add_argument('--debug-colors', action='store_true',
                    help='emit garish hull/accent colours to eyeball the split')
    ap.add_argument('--dark-percentile', type=float, default=DARK_PERCENTILE)
    ap.add_argument('--cache', default=None, help='scratch dir for the decoded texture')
    args = ap.parse_args()

    src = os.path.expanduser(args.src)
    cache = args.cache or tempfile.gettempdir()
    gltf = json.load(open(os.path.join(src, 'scene.gltf')))
    blob = open(os.path.join(src, gltf['buffers'][0]['uri']), 'rb').read()

    pos, nor, uv, tris = collect_mesh(gltf, blob)
    print('source: %d verts, %d triangles' % (len(pos), len(tris)))

    wmap, nweld = weld(pos)
    comps = components(tris, wmap, nweld)
    air = comps[0]
    alo, ahi = bbox(tris, pos, air)
    print('connected components: %d  (largest = airframe, %d tris)' % (len(comps), len(air)))

    # --- 1. landing gear -----------------------------------------------------
    # The gear assemblies are separate shells hanging under the airframe. A
    # component is gear when it is not the airframe and sits entirely below the
    # airframe's lower third -- that catches the two main bogies, the nose
    # bogie and the two gear doors, and nothing else.
    ymin, ymax = alo[1], ahi[1]
    cut = ymin + 0.35 * (ymax - ymin)
    gear, kept = [], list(air)
    for ci, comp in enumerate(comps):
        lo, hi = bbox(tris, pos, comp)
        if ci == 0:
            verdict = 'AIRFRAME'
        elif hi[1] < cut:
            verdict = 'GEAR -> removed'
            gear.append(comp)
        else:
            verdict = 'kept'
            kept.extend(comp)
        print('  comp %d %5d tris  x[%7.4f %7.4f] y[%7.4f %7.4f] z[%7.4f %7.4f]  %s'
              % (ci, len(comp), lo[0], hi[0], lo[1], hi[1], lo[2], hi[2], verdict))
    print('  gear cut plane y < %.4f  (airframe y %.4f..%.4f)' % (cut, ymin, ymax))
    if args.keep_gear:
        kept = list(range(len(tris)))
        gear = []
    removed = sum(len(g) for g in gear)
    faces = sorted(kept)
    print('removed %d gear triangles; %d remain' % (removed, len(faces)))

    # --- 1b. the nose-gear well ---------------------------------------------
    # The nose bogie was its own shell, but its bay and retraction linkage are
    # welded into the belly and stay behind as a black boss hanging under the
    # chin. Find anything that dips below the smoothed belly line, cut it out,
    # and cap the hole flush -- "fold in the pads".
    caps = []
    if not args.keep_gear:
        prot = find_gear_well(tris, pos, faces, wmap)
        if prot:
            pf = set(prot)
            before = faces
            faces = [f for f in faces if f not in pf]
            caps = cap_openings(tris, pos, wmap, before, faces)
            print('removed %d protruding gear-well faces; %d remain (+%d cap triangles)'
                  % (len(prot), len(faces), len(caps)))

    # Degenerate faces (zero area) would survive removal invisibly; drop them.
    def area3(a, b, c):
        ux, uy, uz = b[0] - a[0], b[1] - a[1], b[2] - a[2]
        vx, vy, vz = c[0] - a[0], c[1] - a[1], c[2] - a[2]
        cx, cy, cz = uy * vz - uz * vy, uz * vx - ux * vz, ux * vy - uy * vx
        return 0.5 * math.sqrt(cx * cx + cy * cy + cz * cz)

    degen = [ti for ti in faces if area3(*(pos[v] for v in tris[ti])) < 1e-12]
    if degen:
        print('dropping %d degenerate source faces' % len(degen))
        faces = [ti for ti in faces if ti not in set(degen)]
    caps = [c for c in caps if area3(c[0], c[1], c[2]) >= 1e-12]

    # End-to-end integrity check on exactly what will be written out: skin
    # faces plus caps, welded together, counting edges used by a single face.
    def integrity(fs, cs):
        soup = [tuple(pos[v] for v in tris[ti]) for ti in fs]
        soup += [(c[0], c[1], c[2]) for c in cs]
        vids, table = [], {}
        q = 1e6
        for tri in soup:
            ids = []
            for p in tri:
                k = (round(p[0] * q), round(p[1] * q), round(p[2] * q))
                s = table.get(k)
                if s is None:
                    s = table[k] = len(table)
                ids.append(s)
            vids.append(tuple(ids))
        ec = defaultdict(int)
        for a, b, c in vids:
            for e in ((a, b), (b, c), (c, a)):
                ec[(e[0], e[1]) if e[0] < e[1] else (e[1], e[0])] += 1
        return (len(soup), sum(1 for v in ec.values() if v == 1),
                sum(1 for v in ec.values() if v > 2))

    n_src, b_src, nm_src = integrity(range(len(tris)), [])
    n_out, b_out, nm_out = integrity(faces, caps)
    print('integrity: source %d tris, %d open edges, %d non-manifold edges'
          % (n_src, b_src, nm_src))
    print('           output %d tris, %d open edges, %d non-manifold edges'
          % (n_out, b_out, nm_out))

    # --- 2/3. hull vs accent, driven by the source bake ---------------------
    accent = set()
    lum_stats = None
    if not args.no_texture_classify:
        tex = decode_basecolor(src, cache)
        print('baseColor bake decoded at %dx%d; sampling %d faces'
              % (tex[0], tex[1], len(faces)))
        raw = face_luminance(tris, uv, tex)
        # The airframe is exactly mirror-symmetric but its auto-unwrap is not:
        # mirrored charts land on unrelated parts of the atlas and come back
        # with luminances that differ by 40%+ (chart 9 vs its twin: 0.158 vs
        # 0.224). Averaging each face with its mirror twin removes that, so the
        # jet does not end up with a dark patch on one wing only.
        pair, paired = mirror_pairs(tris, pos, faces), 0
        sym = dict.fromkeys(faces, 0.0)
        for ti in faces:
            mi = pair.get(ti)
            if mi is None:
                sym[ti] = raw[ti]
            else:
                sym[ti] = 0.5 * (raw[ti] + raw[mi])
                paired += 1
        print('  mirror-symmetrised %d/%d faces (%.1f%%)'
              % (paired, len(faces), 100.0 * paired / len(faces)))
        raw = sym
        # The bake has baked-in AO and heavy island bleed, so a per-face
        # threshold on its own is salt-and-pepper noise. Diffuse the signal
        # across the face graph first, which keeps coherent dark regions (the
        # canopy, the nozzle cans, the intake lips) and erases the speckle.
        adj, _ = face_adjacency(tris, wmap, faces)
        cur = {ti: raw[ti] for ti in faces}
        for _ in range(SMOOTH_ITERS):
            nxt = {}
            for ti in faces:
                nb = adj.get(ti)
                nxt[ti] = ((cur[ti] + sum(cur[j] for j in nb)) / (1 + len(nb))) if nb else cur[ti]
            cur = nxt
        srt = sorted(cur.values())
        thr = srt[min(len(srt) - 1, int(args.dark_percentile / 100.0 * len(srt)))]
        dark = {ti for ti in faces if cur[ti] < thr}
        # Erase islands: isolated dark specks are bleed, real accents are patches.
        seen, keep_dark = set(), set()
        for s in dark:
            if s in seen:
                continue
            stack, grp = [s], []
            while stack:
                v = stack.pop()
                if v in seen:
                    continue
                seen.add(v)
                grp.append(v)
                for w in adj.get(v, ()):
                    if w in dark and w not in seen:
                        stack.append(w)
            if len(grp) >= MIN_ACCENT_PATCH:
                keep_dark.update(grp)
        # ...and plug pinholes: a light face ringed by accent is accent.
        for ti in faces:
            if ti in keep_dark:
                continue
            nb = adj.get(ti, ())
            if nb and all(j in keep_dark for j in nb):
                keep_dark.add(ti)
        # Regularise the border. Straight off the threshold it zigzags along
        # triangle edges, which at this triangle density is plainly visible as
        # a torn edge on the flank. A couple of majority votes over the face
        # graph pull it back to something that looks drawn rather than diced.
        for _ in range(MASK_SMOOTH_ROUNDS):
            flip_on, flip_off = set(), set()
            for ti in faces:
                nb = adj.get(ti)
                if not nb:
                    continue
                dark_n = sum(1 for j in nb if j in keep_dark)
                if ti in keep_dark:
                    if dark_n * 3 < len(nb):
                        flip_off.add(ti)
                elif dark_n * 3 >= len(nb) * 2:
                    flip_on.add(ti)
            if not flip_on and not flip_off:
                break
            keep_dark |= flip_on
            keep_dark -= flip_off
        accent = keep_dark
        lum_stats = (min(srt), thr, max(srt))
        print('  smoothed bake luminance min %.4f, threshold %.4f (p%.1f), max %.4f'
              % (lum_stats[0], thr, args.dark_percentile, lum_stats[2]))
        print('  accent faces %d / %d (%.1f%%)'
              % (len(accent), len(faces), 100.0 * len(accent) / len(faces)))
    else:
        print('texture classification skipped')

    # --- 4. orient, recentre, scale -----------------------------------------
    lo, hi = bbox(tris, pos, faces)
    span = [hi[k] - lo[k] for k in range(3)]
    # Which end of X is the nose? Compare the frontal cross-section of the
    # outer 8% of each end; the nose is the slender one.
    def end_span(pick):
        zs, ys = [], []
        for ti in faces:
            for v in tris[ti]:
                if pick(pos[v][0]):
                    zs.append(pos[v][2])
                    ys.append(pos[v][1])
        if not zs:
            return 0.0
        return (max(zs) - min(zs)) * (max(ys) - min(ys))

    band = 0.08 * span[0]
    front = end_span(lambda x: x > hi[0] - band)
    back = end_span(lambda x: x < lo[0] + band)
    nose_at_plus_x = front < back
    print('end cross-sections: +X %.5f, -X %.5f -> nose is at %sX'
          % (front, back, '+' if nose_at_plus_x else '-'))

    scale = REF_LENGTH_X / span[0]
    cx = 0.5 * (lo[0] + hi[0])
    cy = 0.5 * (lo[1] + hi[1])
    cz = 0.5 * (lo[2] + hi[2])
    flip = 1.0 if nose_at_plus_x else -1.0
    print('recentre by (%.4f, %.4f, %.4f), uniform scale x%.4f, yaw %s'
          % (-cx, -cy, -cz, scale, '0' if nose_at_plus_x else '180 (nose was -X)'))

    def xf_p(p):
        return (flip * (p[0] - cx) * scale, (p[1] - cy) * scale, flip * (p[2] - cz) * scale)

    def xf_n(n):
        ln = math.sqrt(n[0] ** 2 + n[1] ** 2 + n[2] ** 2) or 1.0
        return (flip * n[0] / ln, n[1] / ln, flip * n[2] / ln)

    # --- build the two groups ------------------------------------------------
    out_groups = []
    for gname, gfaces, matidx in (('Hull', [f for f in faces if f not in accent], 0),
                                  ('Accent_Cockpit', [f for f in faces if f in accent], 1)):
        remap, verts, out_tris = {}, [], []
        for ti in gfaces:
            tri = []
            for v in tris[ti]:
                j = remap.get(v)
                if j is None:
                    j = remap[v] = len(verts)
                    verts.append((xf_p(pos[v]), xf_n(nor[v])))
                tri.append(j)
            out_tris.append(tuple(tri))
        if gname == 'Hull' and caps:
            # the flush lids over the gear wells; new geometry, so new verts
            for p0, p1, p2, nrm in caps:
                base = len(verts)
                for p in (p0, p1, p2):
                    verts.append((xf_p(p), xf_n(nrm)))
                out_tris.append((base, base + 1, base + 2))
        if not out_tris:
            # --no-texture-classify leaves nothing in the accent bucket; an
            # empty primitive is not valid glTF, so drop the node entirely.
            print('  %-15s empty, not emitted' % gname)
            continue
        out_groups.append((gname, matidx, verts, out_tris))
        print('  %-15s %5d tris, %5d verts' % (gname, len(out_tris), len(verts)))

    hull_rgb = (1.0, 0.15, 0.05) if args.debug_colors else tuple(LIN[c] for c in HULL_SRGB)
    acc_rgb = (0.02, 0.9, 0.25) if args.debug_colors else tuple(LIN[c] for c in ACCENT_SRGB)

    def luma(c):
        return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]

    materials = [
        {'name': 'hull', 'doubleSided': False,
         'pbrMetallicRoughness': {'baseColorFactor': list(hull_rgb) + [1.0],
                                  'metallicFactor': HULL_METALLIC,
                                  'roughnessFactor': HULL_ROUGHNESS}},
        {'name': 'accent', 'doubleSided': False,
         'pbrMetallicRoughness': {'baseColorFactor': list(acc_rgb) + [1.0],
                                  'metallicFactor': ACCENT_METALLIC,
                                  'roughnessFactor': ACCENT_ROUGHNESS}},
    ]
    print('materials: hull linear %s luma %.4f | accent linear %s luma %.4f'
          % (tuple(round(v, 4) for v in hull_rgb), luma(hull_rgb),
             tuple(round(v, 4) for v in acc_rgb), luma(acc_rgb)))
    if not args.debug_colors:
        assert luma(hull_rgb) > 0.35 > luma(acc_rgb), 'materials fall on the wrong side of 0.35'

    extras = {
        'title': 'Modern Jet Fighter Low Poly Game Ready Free',
        'author': 'Hdjusj (https://sketchfab.com/Hdjusj)',
        'license': 'CC-BY-4.0 (http://creativecommons.org/licenses/by/4.0/)',
        'source': ('https://sketchfab.com/3d-models/modern-jet-fighter-low-poly-game-ready-'
                   'free-cd2bd715dcd14dc4b47ebaeb2403fb89'),
        'credit': CREDIT,
        'notes': 'Landing gear removed, baseColor bake discarded, split into hull/accent.',
    }

    out = os.path.abspath(args.out)
    os.makedirs(os.path.dirname(out), exist_ok=True)
    size = write_glb(out, out_groups, materials, extras)

    nlo = [1e30] * 3
    nhi = [-1e30] * 3
    for _n, _m, verts, _t in out_groups:
        for (p, _) in verts:
            for k in range(3):
                nlo[k] = min(nlo[k], p[k])
                nhi[k] = max(nhi[k], p[k])
    bad = 0
    for _n, _m, verts, gtris in out_groups:
        for t in gtris:
            if len(set(t)) < 3 or area3(*(verts[i][0] for i in t)) < 1e-12:
                bad += 1
    print('wrote %s (%.1f KB)' % (out, size / 1024.0))
    print('  degenerate triangles in output: %d' % bad)
    assert bad == 0, 'degenerate geometry made it into the output'
    print('  bounds  %.4f x %.4f x %.4f  (reference ship %.4f x %.4f x %.4f)'
          % (nhi[0] - nlo[0], nhi[1] - nlo[1], nhi[2] - nlo[2],
             REF_LENGTH_X, REF_HEIGHT_Y, REF_WIDTH_Z))
    print('  centre  (%.5f, %.5f, %.5f)'
          % tuple(0.5 * (nlo[k] + nhi[k]) for k in range(3)))
    print('  triangles %d' % sum(len(g[3]) for g in out_groups))


if __name__ == '__main__':
    main()
