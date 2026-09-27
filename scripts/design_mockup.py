"""Design direction mockup — assembles a screen into PNG using only what a terminal can really draw
(cells·truecolor·bold/italic·block/line chars). The reference image for porting the chosen direction to the
real renderer (term.rs) and built-in themes.

    uv run --with pillow python3 scripts/design_mockup.py docs/design
"""
import os, re, sys
from PIL import Image, ImageDraw, ImageFont

COLS, ROWS = 100, 27
S = 2  # resolution scale
SIZE = 15 * S
FONTS = os.path.expanduser("~/Library/Fonts")
mono = ImageFont.truetype(f"{FONTS}/JetBrainsMono-Regular.ttf", SIZE)
mono_b = ImageFont.truetype(f"{FONTS}/JetBrainsMono-SemiBold.ttf", SIZE)
mono_i = ImageFont.truetype(f"{FONTS}/JetBrainsMono-Italic.ttf", SIZE)
hangul = ImageFont.truetype("/System/Library/Fonts/AppleSDGothicNeo.ttc", SIZE)
CW = int(mono.getlength("M"))
CH = int(SIZE * 1.5)


def hexc(h):
    h = h.lstrip("#")
    return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))


def mix(a, b, t):
    a, b = hexc(a) if isinstance(a, str) else a, hexc(b) if isinstance(b, str) else b
    return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(3))


def wide(ch):
    return ord(ch) >= 0x1100 and not (0x2500 <= ord(ch) <= 0x25FF) and ch not in "…·›•▎▏"


class Grid:
    def __init__(self, bg, fg):
        self.bg, self.fg = hexc(bg), hexc(fg)
        self.c = [[[" ", self.fg, self.bg, "", False] for _ in range(COLS)] for _ in range(ROWS)]

    def put(self, x, y, text, fg=None, bg=None, style=""):
        for ch in text:
            if not (0 <= x < COLS and 0 <= y < ROWS):
                x += 1
                continue
            cell = self.c[y][x]
            cell[0] = ch
            if fg is not None:
                cell[1] = hexc(fg) if isinstance(fg, str) else fg
            if bg is not None:
                cell[2] = hexc(bg) if isinstance(bg, str) else bg
            cell[3] = style
            if wide(ch) and x + 1 < COLS:
                nxt = self.c[y][x + 1]
                nxt[0] = ""
                if bg is not None:
                    nxt[2] = cell[2]
                x += 2
            else:
                x += 1
        return x

    def fill(self, x, y, w, h, bg):
        for yy in range(y, y + h):
            for xx in range(x, x + w):
                if 0 <= xx < COLS and 0 <= yy < ROWS:
                    self.c[yy][xx] = [" ", self.fg, hexc(bg) if isinstance(bg, str) else bg, "", False]

    def spans(self, x, y, spans, bg=None):
        for text, fg, *st in spans:
            x = self.put(x, y, text, fg, bg, st[0] if st else "")
        return x

    def render(self, path, cursor=None, cursor_color=None):
        pad = 16 * S
        img = Image.new("RGB", (COLS * CW + pad * 2, ROWS * CH + pad * 2), self.bg)
        d = ImageDraw.Draw(img)
        for y in range(ROWS):
            for x in range(COLS):
                ch, fg, bg, st, _ = self.c[y][x]
                if bg != self.bg:
                    px, py = pad + x * CW, pad + y * CH
                    d.rectangle([px, py, px + CW - 1, py + CH - 1], fill=bg)
        for y in range(ROWS):
            for x in range(COLS):
                ch, fg, bg, st, _ = self.c[y][x]
                if not ch.strip():
                    continue
                px, py = pad + x * CW, pad + y * CH
                blocks = {"▀": (0, 0, 1, .5), "▄": (0, .5, 1, 1), "▌": (0, 0, .5, 1), "▐": (.5, 0, 1, 1),
                          "█": (0, 0, 1, 1), "▎": (0, 0, .25, 1), "▏": (0, 0, .125, 1)}
                if ch in blocks:
                    a, b, c2, e = blocks[ch]
                    d.rectangle([px + a * CW, py + b * CH, px + c2 * CW - 1, py + e * CH - 1], fill=fg)
                    continue
                lines = {"─": "lr", "│": "ud", "├": "udr", "┤": "udl"}
                mx, my = px + CW // 2, py + CH // 2
                if ch in lines:
                    for dd in lines[ch]:
                        seg = {"l": [px, my, mx, my], "r": [mx, my, px + CW, my], "u": [mx, py, mx, my],
                               "d": [mx, my, mx, py + CH]}[dd]
                        d.line(seg, fill=fg, width=S)
                    continue
                arcs = {"╭": ((mx, my, mx + CW, my + CH), 180, 270, [mx, my + CH // 2, mx, py + CH], [mx + CW // 2, my, px + CW, my]),
                        "╮": ((mx - CW, my, mx, my + CH), 270, 360, [mx, my + CH // 2, mx, py + CH], [px, my, mx - CW // 2, my]),
                        "╰": ((mx, my - CH, mx + CW, my), 90, 180, [mx, py, mx, my - CH // 2], [mx + CW // 2, my, px + CW, my]),
                        "╯": ((mx - CW, my - CH, mx, my), 0, 90, [mx, py, mx, my - CH // 2], [px, my, mx - CW // 2, my])}
                if ch in arcs:
                    box, a0, a1, l1, l2 = arcs[ch]
                    r = min(CW, CH) // 2
                    cx0 = {"╭": (mx + r, my + r), "╮": (mx - r, my + r), "╰": (mx + r, my - r), "╯": (mx - r, my - r)}[ch]
                    d.arc([cx0[0] - r, cx0[1] - r, cx0[0] + r, cx0[1] + r], a0, a1, fill=fg, width=S)
                    if ch in "╭╮":
                        d.line([mx, cx0[1], mx, py + CH], fill=fg, width=S)
                    else:
                        d.line([mx, py, mx, cx0[1]], fill=fg, width=S)
                    if ch in "╭╰":
                        d.line([cx0[0], my, px + CW, my], fill=fg, width=S)
                    else:
                        d.line([px, my, cx0[0], my], fill=fg, width=S)
                    continue
                f = hangul if wide(ch) else (mono_b if "b" in st else mono_i if "i" in st else mono)
                d.text((px, py + 4 * S), ch, font=f, fill=fg)
                if "u" in st:
                    d.line([px, py + CH - 4 * S, px + CW - 1, py + CH - 4 * S], fill=fg, width=S)
                if "~" in st:  # curly underline
                    for k in range(0, CW, 2):
                        yy = py + CH - 4 * S + (S if (k // 3) % 2 else -S)
                        d.point((px + k, yy), fill=fg)
        if cursor:
            x, y = cursor
            d.rectangle([pad + x * CW, pad + y * CH + 3 * S, pad + x * CW + 2 * S - 1, pad + (y + 1) * CH - 3 * S], fill=cursor_color)
        img.save(path)
        print("saved", path, img.size)


CODE = """use std::collections::HashMap;

/// 실타래 — 이어진 실 한 가닥.
struct Thread {
    name: String,
    knots: Vec<u32>,
}

fn main() {
    let mut threads: HashMap<String, Thread> = HashMap::new();
    let first = Thread { name: "타래".into(), knots: vec![1, 2, 3] };
    threads.insert(first.name.clone(), first);
    threads.ge
}""".split("\n")

KW = {"use", "struct", "fn", "let", "mut", "pub", "impl"}


def tokenize(line, P):
    """(text, color, style) — rough Rust highlighting (for the mockup)."""
    out = []
    if line.strip().startswith("///"):
        return [(line, P["comment"], "i")]
    for m in re.finditer(r'"[^"]*"|[A-Za-z_][A-Za-z_0-9]*!?|\d+|::|->|\s+|.', line):
        t = m.group()
        after = line[m.end():m.end() + 1]
        if t.startswith('"'):
            c, s = P["string"], ""
        elif t in KW:
            c, s = P["keyword"], ""
        elif t.endswith("!"):
            c, s = P["macro"], ""
        elif t.isdigit():
            c, s = P["number"], ""
        elif t[0].isupper():
            c, s = P["type"], ""
        elif after == "(":
            c, s = P["function"], ""
        elif after == ":" and line[m.end():m.end() + 2] != "::" and t not in ("mut",):
            c, s = P["field"], ""
        elif t in "{}()[];,.<>:=&" or t in ("::", "->"):
            c, s = P["punct"], ""
        else:
            c, s = P["fg"], ""
        out.append((t, c, s))
    return out


ITEMS = [("fn", "get", "Option<&V>"), ("fn", "get_mut", "Option<&mut V>"), ("fn", "get_key_value", "Option<(&K, &V)>"),
         ("fn", "get_disjoint_mut", "[Option; N]"), ("method", "iter().ge", "as Iterator")]
DOCS_SIG = "fn get<Q>(&self, k: &Q) -> Option<&V>"
DOCS = ["Returns a reference to the value", "corresponding to the key.", "",
        "The key may be any borrowed form of", "the map's key type."]


def editor(g, P, *, gutter_style, cursor_row=12):
    top = 1
    for i, line in enumerate(CODE):
        y = top + i
        rel = abs(i - cursor_row)
        is_cur = i == cursor_row
        if is_cur and P.get("cursorline"):
            g.fill(0, y, COLS, 1, P["cursorline"])
        num = str(i + 1) if gutter_style == "abs" else (str(i + 1) if is_cur else str(rel))
        g.put(4 - len(num), y, num, P["linenr_cur"] if is_cur else P["linenr"], None, "b" if is_cur else "")
        if i == 5:
            g.put(0, y, "●" if gutter_style == "abs" else "▎", P["warn"])
        g.spans(6, y, [(t, c, s) for t, c, s in tokenize(line, P)])
    # Warning underline (knots)
    for x in range(10, 15):
        g.c[top + 5][x][3] = "~"
        g.c[top + 5][x][1] = P["fg"]
    return top


def completion(g, P, *, y, word_x, kind_style, card, selected, sel_bar, hit, detail, docs_card, docs_bg):
    kinds = {"fn": ("ƒ", P["function"]), "method": ("ƒ", P["function"])}
    menu_x = word_x - 4
    w = 38
    rows = len(ITEMS)
    if card == "halfblock":
        g.put(menu_x, y, "▄" * w, P["surface"])
        body = y + 1
    else:
        body = y
    for r, (k, label, det) in enumerate(ITEMS):
        yy = body + r
        is_sel = r == 0
        bg = selected if is_sel else P["surface"]
        g.fill(menu_x, yy, w, 1, bg)
        if is_sel and sel_bar:
            g.put(menu_x, yy, "▎", sel_bar)
        if kind_style == "glyph":
            glyph, col = kinds[k]
            g.put(menu_x + 2, yy, glyph, col)
        else:
            g.put(menu_x + 1, yy, k.rjust(2) if k == "fn" else "fn", mix(P["function"], P["surface"], .35))
        x = menu_x + 4
        typed = 2 if label.startswith("ge") else 0
        for j, ch in enumerate(label):
            h = (label.startswith("ge") and j < 2) or (label.startswith("iter") and label[j:j + 2] == "ge")
            if label.startswith("iter") and j >= 1 and label[j - 1:j + 1] == "ge":
                h = True
            g.put(x, yy, ch, hit if h else (P["fg_strong"] if is_sel else P["fg"]), None, "b" if (h or is_sel) else "")
            x += 1
        g.put(menu_x + w - 1 - len(det), yy, det, detail)
    end = body + rows
    if card == "halfblock":
        g.put(menu_x, end, "▀" * w, P["surface"])
    # Description card
    dx, dw = menu_x + w + 2, 44
    dy = y
    lines = [("sig", DOCS_SIG)] + [("", "")] + [("t", t) for t in DOCS]
    if docs_card == "halfblock":
        g.put(dx, dy, "▄" * dw, docs_bg)
        for i, (kind, text) in enumerate(lines):
            g.fill(dx, dy + 1 + i, dw, 1, docs_bg)
        g.put(dx, dy + 1 + len(lines), "▀" * dw, docs_bg)
        base = dy + 1
    else:  # border
        g.fill(dx, dy, dw, len(lines) + 2, docs_bg)
        g.put(dx, dy, "╭" + "─" * (dw - 2) + "╮", P["border"])
        for i in range(len(lines)):
            g.fill(dx, dy + 1 + i, dw, 1, docs_bg)
            g.put(dx, dy + 1 + i, "│", P["border"], docs_bg)
            g.put(dx + dw - 1, dy + 1 + i, "│", P["border"], docs_bg)
        g.put(dx, dy + 1 + len(lines), "╰" + "─" * (dw - 2) + "╯", P["border"], docs_bg)
        base = dy + 1
    for i, (kind, text) in enumerate(lines):
        if kind == "sig":
            g.spans(dx + 2, base + i, [(t, c, s) for t, c, s in tokenize(text, P)], docs_bg)
        else:
            g.put(dx + 2, base + i, text, P["doc_fg"], docs_bg)


def statusline(g, P, y, *, kind):
    if kind == "pill":
        g.fill(0, y, COLS, 1, P["bg"])
        x = g.put(1, y, "▐", P["accent"])
        x = g.put(x, y, " INSERT ", P["on_accent"], P["accent"], "b")
        x = g.put(x, y, "▌", P["accent"])
        x = g.spans(x + 1, y, [("main.rs", P["fg_strong"], "b"), ("  ", P["dim"]), ("● ", P["accent"]), ("tarae-ra-demo", P["dim"]), (" · ", P["faint"]), ("main", P["dim"])])
        right = [("▲ 1", P["warn"]), ("   ", P["dim"]), ("rust", P["dim"]), ("   ", P["dim"]), ("utf-8", P["dim"]), ("   ", P["dim"]), ("13:15", P["fg"])]
        w = sum(len(t) for t, _ in right)
        g.spans(COLS - 1 - w, y, right)
    elif kind == "gradient":
        stops = P["gradient"]
        for x in range(COLS):
            t = x / (COLS - 1)
            seg = min(int(t * (len(stops) - 1)), len(stops) - 2)
            lt = t * (len(stops) - 1) - seg
            c = mix(stops[seg], stops[seg + 1], lt)
            g.fill(x, y, 1, 1, mix(c, P["bg"], .82))
        x = g.put(0, y, " INSERT ", P["bg"], hexc(stops[0]), "b")
        x = g.put(x, y, "▌", hexc(stops[0]), None)
        g.spans(x + 1, y, [("main.rs", P["fg_strong"], "b"), ("  tarae-ra-demo · main", P["dim"])])
        right = [("▲ 1", P["warn"]), ("  rust  utf-8  ", P["dim"]), ("13:15 ", P["fg_strong"])]
        w = sum(len(t) for t, _ in right)
        g.spans(COLS - w, y, right)
    else:  # hanji: over a thin line
        g.put(0, y - 1, "─" * COLS, P["faint"])
        x = g.put(1, y, "● ", P["accent"])
        x = g.put(x, y, "insert", P["accent"], None, "b")
        g.spans(x + 3, y, [("main.rs", P["fg_strong"], "b"), ("  ·  tarae-ra-demo  ·  main", P["dim"])])
        right = [("▲ 1", P["warn"]), ("  ·  rust  ·  utf-8  ·  ", P["dim"]), ("13:15", P["fg"])]
        w = sum(len(t) for t, _ in right)
        g.spans(COLS - 1 - w, y, right)


def breadcrumb(g, P, *, bg=None):
    g.spans(2, 0, [("tarae-ra-demo", P["dim"]), ("  ›  ", P["faint"]), ("src", P["dim"]), ("  ›  ", P["faint"]),
                   ("main.rs", P["fg_strong"], "b"), ("  ›  ", P["faint"]), ("fn main", P["dim"])], bg)


# ── Direction A: 먹 (Ink) — ink background + a single thread-coral accent ─────────────
INK = dict(
    bg="#101217", fg="#d6d2c8", fg_strong="#f1ede4", dim="#747884", faint="#353944", punct="#8d909a",
    surface="#1a1d24", selected="#262a34", border="#2c303a", doc_fg="#a9a59c",
    accent="#ef8a5a", on_accent="#101217", warn="#e3b660",
    keyword="#b39cf2", function="#7fb2ec", type="#e2c27f", string="#9bcf8e", number="#ec9f76",
    comment="#5d6270", field="#d6d2c8", macro="#78c8c8", linenr="#353944", linenr_cur="#ef8a5a",
    cursorline="#15181e",
)
g = Grid(INK["bg"], INK["fg"])
breadcrumb(g, INK)
top = editor(g, INK, gutter_style="rel")
completion(g, INK, y=top + 13, word_x=6 + 12, kind_style="glyph", card="halfblock", selected=INK["selected"],
           sel_bar=INK["accent"], hit=INK["accent"], detail=INK["dim"], docs_card="halfblock", docs_bg=INK["surface"])
statusline(g, INK, ROWS - 2, kind="pill")
g.put(1, ROWS - 1, "tab", INK["dim"], None, "b")
g.spans(5, ROWS - 1, [("select  ", INK["faint"]), ("enter", INK["dim"], "b"), (" accept  ", INK["faint"]), ("C-d", INK["dim"], "b"), (" docs", INK["faint"])])
out = sys.argv[1] if len(sys.argv) > 1 else "docs/design"
os.makedirs(out, exist_ok=True)
g.render(f"{out}/a-ink.png", cursor=(6 + 14, top + 12), cursor_color=hexc(INK["accent"]))

# ── Direction B: 한지 (Paper) — light paper + seal vermilion ─────────────────────
PAPER = dict(
    bg="#f5f1e8", fg="#34322d", fg_strong="#1d1c19", dim="#8d887c", faint="#cfc9bb", punct="#7d786d",
    surface="#ebe5d8", selected="#ded6c4", border="#d3ccbc", doc_fg="#5f5b52",
    accent="#c4452c", on_accent="#f5f1e8", warn="#b07d12",
    keyword="#8a3f9c", function="#2c5d99", type="#8a6414", string="#3f7a3a", number="#b4501f",
    comment="#a9a397", field="#34322d", macro="#1f7d7d", linenr="#d3ccbc", linenr_cur="#c4452c",
    cursorline="#efeadf",
)
g = Grid(PAPER["bg"], PAPER["fg"])
breadcrumb(g, PAPER)
top = editor(g, PAPER, gutter_style="abs")
completion(g, PAPER, y=top + 13, word_x=6 + 12, kind_style="glyph", card="halfblock", selected=PAPER["selected"],
           sel_bar=PAPER["accent"], hit=PAPER["accent"], detail=PAPER["dim"], docs_card="halfblock", docs_bg=PAPER["surface"])
statusline(g, PAPER, ROWS - 1, kind="paper")
g.render(f"{out}/b-paper.png", cursor=(6 + 14, top + 12), cursor_color=hexc(PAPER["accent"]))

# ── Direction C: Aurora — deep night + gradients ───────────────────────
AUR = dict(
    bg="#0c0e16", fg="#c3cdf0", fg_strong="#eef1ff", dim="#6b7394", faint="#2a2f45", punct="#8891b8",
    surface="#141827", selected="#1f2540", border="#5b6bd6", doc_fg="#98a2c8",
    accent="#8aa2ff", on_accent="#0c0e16", warn="#ffc777",
    keyword="#c099ff", function="#82aaff", type="#65bcff", string="#c3e88d", number="#ff966c",
    comment="#5a6389", field="#c3cdf0", macro="#86e1fc", linenr="#2a2f45", linenr_cur="#c099ff",
    cursorline="#11141f", gradient=["#82aaff", "#c099ff", "#ff757f"],
)
g = Grid(AUR["bg"], AUR["fg"])
breadcrumb(g, AUR)
top = editor(g, AUR, gutter_style="rel")
completion(g, AUR, y=top + 13, word_x=6 + 12, kind_style="glyph", card="flat", selected=AUR["selected"],
           sel_bar=AUR["keyword"], hit=AUR["keyword"], detail=AUR["dim"], docs_card="border", docs_bg=AUR["surface"])
statusline(g, AUR, ROWS - 2, kind="gradient")
g.render(f"{out}/c-aurora.png", cursor=(6 + 14, top + 12), cursor_color=hexc(AUR["keyword"]))
