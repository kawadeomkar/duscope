"""Cut a TrueType (glyf) font down to a set of code points: how
crates/duscope-treemap/src/font/Inter-Regular-subset.ttf was made.

Composite glyphs are flattened into simple ones, hinting is dropped, and
only the tables a renderer of simple glyphs needs are kept: cmap (format 4),
glyf, head, hhea, hmtx, loca, maxp, name, OS/2, post (format 3).

The source: extras/ttf/Inter-Regular.ttf in Inter-4.1.zip, the v4.1 release
of github.com/rsms/inter (commit e3a3d4c57d5ecc01453a575621882a384c1995a3;
the zip's SHA-256 is
9883fdd4a49d4fb66bd8177ba6625ef9a64aa45899767dde3d36aa425756b11e). It needs
the Python 3 standard library alone.

usage: python3 scripts/subset_font.py IN.ttf OUT.ttf
"""

import struct
import sys

CODE_POINTS = (
    list(range(0x20, 0x7F))
    + list(range(0xA0, 0x100))
    + [0x2013, 0x2014, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2026, 0x20AC, 0xFFFD]
)


def tables(data):
    num = struct.unpack(">H", data[4:6])[0]
    out = {}
    for i in range(num):
        tag, _check, off, length = struct.unpack(">4sIII", data[12 + 16 * i : 28 + 16 * i])
        out[tag.decode("latin-1")] = data[off : off + length]
    return out


def cmap_lookup(cmap):
    """code point -> glyph id, from the (3,10) format 12 or (3,1) format 4."""
    num = struct.unpack(">H", cmap[2:4])[0]
    subs = {}
    for i in range(num):
        pid, eid, off = struct.unpack(">HHI", cmap[4 + 8 * i : 12 + 8 * i])
        subs[(pid, eid)] = off
    mapping = {}
    if (3, 10) in subs:
        off = subs[(3, 10)]
        n = struct.unpack(">I", cmap[off + 12 : off + 16])[0]
        for g in range(n):
            start, end, gid = struct.unpack(">III", cmap[off + 16 + 12 * g : off + 28 + 12 * g])
            for c in range(start, end + 1):
                mapping[c] = gid + (c - start)
        return mapping
    off = subs[(3, 1)]
    segx2 = struct.unpack(">H", cmap[off + 6 : off + 8])[0]
    seg = segx2 // 2
    ends = struct.unpack(">%dH" % seg, cmap[off + 14 : off + 14 + segx2])
    starts = struct.unpack(">%dH" % seg, cmap[off + 16 + segx2 : off + 16 + 2 * segx2])
    deltas = struct.unpack(">%dh" % seg, cmap[off + 16 + 2 * segx2 : off + 16 + 3 * segx2])
    ro_at = off + 16 + 3 * segx2
    ranges = struct.unpack(">%dH" % seg, cmap[ro_at : ro_at + segx2])
    for i in range(seg):
        for c in range(starts[i], ends[i] + 1):
            if c == 0xFFFF:
                continue
            if ranges[i] == 0:
                gid = (c + deltas[i]) & 0xFFFF
            else:
                at = ro_at + 2 * i + ranges[i] + 2 * (c - starts[i])
                gid = struct.unpack(">H", cmap[at : at + 2])[0]
                if gid:
                    gid = (gid + deltas[i]) & 0xFFFF
            if gid:
                mapping[c] = gid
    return mapping


def glyph_data(glyf, loca, long_loca, gid):
    if long_loca:
        a, b = struct.unpack(">II", loca[4 * gid : 4 * gid + 8])
    else:
        a, b = struct.unpack(">HH", loca[2 * gid : 2 * gid + 4])
        a, b = 2 * a, 2 * b
    return glyf[a:b]


def contours_of(glyf, loca, long_loca, gid, depth=0):
    """[[(x, y, on_curve), ...], ...] of a glyph, composites flattened."""
    data = glyph_data(glyf, loca, long_loca, gid)
    if not data:
        return []
    n = struct.unpack(">h", data[0:2])[0]
    if n >= 0:
        ends = struct.unpack(">%dH" % n, data[10 : 10 + 2 * n])
        points = ends[-1] + 1 if n else 0
        at = 10 + 2 * n
        ilen = struct.unpack(">H", data[at : at + 2])[0]
        at += 2 + ilen
        flags = []
        while len(flags) < points:
            f = data[at]
            at += 1
            flags.append(f)
            if f & 8:
                r = data[at]
                at += 1
                flags.extend([f] * r)
        xs = []
        x = 0
        for f in flags:
            if f & 2:
                d = data[at]
                at += 1
                x += d if f & 16 else -d
            elif not f & 16:
                x += struct.unpack(">h", data[at : at + 2])[0]
                at += 2
            xs.append(x)
        ys = []
        y = 0
        for f in flags:
            if f & 4:
                d = data[at]
                at += 1
                y += d if f & 32 else -d
            elif not f & 32:
                y += struct.unpack(">h", data[at : at + 2])[0]
                at += 2
            ys.append(y)
        out = []
        start = 0
        for e in ends:
            out.append([(xs[i], ys[i], bool(flags[i] & 1)) for i in range(start, e + 1)])
            start = e + 1
        return out
    # A composite: each component's contours, moved and scaled.
    assert depth < 8, "composite too deep"
    at = 10
    out = []
    while True:
        flags, cgid = struct.unpack(">HH", data[at : at + 4])
        at += 4
        if flags & 1:
            dx, dy = struct.unpack(">hh", data[at : at + 4])
            at += 4
        else:
            dx, dy = struct.unpack(">bb", data[at : at + 2])
            at += 2
        assert flags & 2, "point-matched components are not handled"
        a, b, c, d = 1.0, 0.0, 0.0, 1.0
        if flags & 8:
            a = d = struct.unpack(">h", data[at : at + 2])[0] / 16384.0
            at += 2
        elif flags & 0x40:
            a, d = (v / 16384.0 for v in struct.unpack(">hh", data[at : at + 4]))
            at += 4
        elif flags & 0x80:
            a, b, c, d = (v / 16384.0 for v in struct.unpack(">hhhh", data[at : at + 8]))
            at += 8
        for contour in contours_of(glyf, loca, long_loca, cgid, depth + 1):
            out.append(
                [
                    (round(a * x + c * y + dx), round(b * x + d * y + dy), on)
                    for (x, y, on) in contour
                ]
            )
        if not flags & 0x20:
            break
    return out


def encode_glyph(contours):
    if not contours:
        return b""
    pts = [p for c in contours for p in c]
    xs = [p[0] for p in pts]
    ys = [p[1] for p in pts]
    head = struct.pack(">hhhhh", len(contours), min(xs), min(ys), max(xs), max(ys))
    ends = []
    n = 0
    for c in contours:
        n += len(c)
        ends.append(n - 1)
    out = head + struct.pack(">%dH" % len(ends), *ends) + struct.pack(">H", 0)
    flags, xb, yb = [], b"", b""
    px = py = 0
    for x, y, on in pts:
        f = 1 if on else 0
        dx, dy = x - px, y - py
        px, py = x, y
        if dx == 0:
            f |= 16
        elif -255 <= dx <= 255:
            f |= 2 | (16 if dx > 0 else 0)
            xb += bytes([abs(dx)])
        else:
            xb += struct.pack(">h", dx)
        if dy == 0:
            f |= 32
        elif -255 <= dy <= 255:
            f |= 4 | (32 if dy > 0 else 0)
            yb += bytes([abs(dy)])
        else:
            yb += struct.pack(">h", dy)
        flags.append(f)
    fb = b""
    i = 0
    while i < len(flags):
        f = flags[i]
        r = 0
        while i + r + 1 < len(flags) and flags[i + r + 1] == f and r < 255:
            r += 1
        if r:
            fb += bytes([f | 8, r])
        else:
            fb += bytes([f])
        i += r + 1
    out += fb + xb + yb
    if len(out) % 2:
        out += b"\0"
    return out


def cmap_format4(mapping):
    """A cmap with one format 4 subtable, for (0,3) and (3,1)."""
    codes = sorted(mapping)
    segs = []
    for c in codes:
        if segs and c == segs[-1][1] + 1 and mapping[c] == mapping[segs[-1][1]] + 1:
            segs[-1][1] = c
        else:
            segs.append([c, c])
    segs.append([0xFFFF, 0xFFFF])
    n = len(segs)
    search = 2 * (1 << (n.bit_length() - 1))
    starts = [s for s, _ in segs]
    ends = [e for _, e in segs]
    deltas = [((mapping[s] - s) & 0xFFFF) if s != 0xFFFF else 1 for s, _ in segs]
    body = struct.pack(">HHHH", 2 * n, search, (n.bit_length() - 1), 2 * n - search)
    body += struct.pack(">%dH" % n, *ends) + b"\0\0"
    body += struct.pack(">%dH" % n, *starts)
    body += struct.pack(">%dH" % n, *deltas)
    body += struct.pack(">%dH" % n, *([0] * n))
    sub = struct.pack(">HHH", 4, 6 + len(body), 0) + body
    return struct.pack(">HHHHIHHI", 0, 2, 0, 3, 20, 3, 1, 20) + sub


def checksum(data):
    data += b"\0" * ((4 - len(data) % 4) % 4)
    return sum(struct.unpack(">%dI" % (len(data) // 4), data)) & 0xFFFFFFFF


def main(src, dst):
    data = open(src, "rb").read()
    t = tables(data)
    head = bytearray(t["head"])
    long_loca = struct.unpack(">h", head[50:52])[0] == 1
    num_glyphs = struct.unpack(">H", t["maxp"][4:6])[0]
    hhea = bytearray(t["hhea"])
    nhm = struct.unpack(">H", hhea[34:36])[0]
    hmtx = t["hmtx"]
    advance = [struct.unpack(">H", hmtx[4 * min(g, nhm - 1) : 4 * min(g, nhm - 1) + 2])[0] for g in range(num_glyphs)]
    mapping = cmap_lookup(t["cmap"])
    order = [0]
    new_map = {}
    for c in CODE_POINTS:
        if c not in mapping:
            print("missing U+%04X" % c, file=sys.stderr)
            continue
        gid = mapping[c]
        if gid not in order:
            order.append(gid)
        new_map[c] = order.index(gid)
    glyf = b""
    loca = []
    hm = b""
    max_points = max_contours = 0
    for gid in order:
        contours = contours_of(t["glyf"], t["loca"], long_loca, gid)
        enc = encode_glyph(contours)
        loca.append(len(glyf))
        glyf += enc
        xmin = min((p[0] for c in contours for p in c), default=0)
        hm += struct.pack(">Hh", advance[gid], xmin)
        max_points = max(max_points, sum(len(c) for c in contours))
        max_contours = max(max_contours, len(contours))
    loca.append(len(glyf))
    n = len(order)
    struct.pack_into(">h", head, 50, 1)
    struct.pack_into(">I", head, 8, 0)
    struct.pack_into(">H", hhea, 34, n)
    maxp = bytearray(t["maxp"])
    struct.pack_into(">H", maxp, 4, n)
    if len(maxp) >= 32:
        struct.pack_into(">HH", maxp, 6, max_points, max_contours)
        struct.pack_into(">HH", maxp, 10, 0, 0)  # composite points, contours
        struct.pack_into(">H", maxp, 28, 0)  # component elements
        struct.pack_into(">H", maxp, 30, 0)  # component depth
        struct.pack_into(">H", maxp, 26, 0)  # instruction size
    post = bytearray(t["post"][:32])
    struct.pack_into(">I", post, 0, 0x00030000)
    out_tables = {
        "OS/2": t["OS/2"],
        "cmap": cmap_format4(new_map),
        "glyf": glyf,
        "head": bytes(head),
        "hhea": bytes(hhea),
        "hmtx": hm,
        "loca": struct.pack(">%dI" % len(loca), *loca),
        "maxp": bytes(maxp),
        "name": t["name"],
        "post": bytes(post),
    }
    tags = sorted(out_tables)
    num = len(tags)
    search = 16 * (1 << (num.bit_length() - 1))
    header = struct.pack(">IHHHH", 0x00010000, num, search, num.bit_length() - 1, 16 * num - search)
    offset = 12 + 16 * num
    directory = b""
    body = b""
    for tag in tags:
        tb = out_tables[tag]
        directory += struct.pack(">4sIII", tag.encode(), checksum(tb), offset + len(body), len(tb))
        body += tb + b"\0" * ((4 - len(tb) % 4) % 4)
    font = bytearray(header + directory + body)
    adjust = (0xB1B0AFBA - checksum(bytes(font))) & 0xFFFFFFFF
    head_at = offset + sum(len(out_tables[x]) + (4 - len(out_tables[x]) % 4) % 4 for x in tags[: tags.index("head")])
    struct.pack_into(">I", font, head_at + 8, adjust)
    open(dst, "wb").write(bytes(font))
    print("%d glyphs, %d bytes" % (n, len(font)))


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
