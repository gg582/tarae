"""tarae screenshot — renders the screen actually produced in a virtual terminal (pyte) to PNG.

    uv run --with pyte --with pillow python3 scripts/screenshot.py out.png [keys ...]

Keys are sent in order (\\r = Enter, \\x1b = Esc). Config is tarae/config.toml under $TARAE_SHOT_XDG
(default /tmp/tarae-shot/xdg). Line chars (│─╭…) become cell-filling lines like a real terminal;
backgrounds are painted before glyphs.
"""
import os, re, pty, time, select, fcntl, termios, struct, sys, glob
import pyte
from PIL import Image, ImageDraw, ImageFont

COLS, ROWS = 112, 30
keys = sys.argv[2:] if len(sys.argv) > 2 else []
out_png = sys.argv[1]

REPLAY = os.environ.get("TARAE_SHOT_REPLAY")  # only draw terminal bytes recorded by another script
pid, m = (1, None) if REPLAY else pty.fork()
if pid == 0:
    os.environ["TERM"] = "xterm-256color"; os.environ["COLORTERM"] = "truecolor"
    os.environ["TARAE_UNDERCURL"] = "0"   # pyte doesn't know CSI 4:3 m
    os.environ["XDG_CONFIG_HOME"] = os.environ.get("TARAE_SHOT_XDG", "/tmp/tarae-shot/xdg")
    os.chdir(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
    files = os.environ.get("TARAE_SHOT_FILES", "src/transaction.rs src/selection.rs").split()
    tarae = os.path.abspath("target/release/tarae")
    if os.environ.get("TARAE_SHOT_CWD"):
        os.chdir(os.environ["TARAE_SHOT_CWD"])
    os.execv(tarae, ["tarae", *files])
if REPLAY:
    COLS, ROWS = int(os.environ.get("TARAE_SHOT_COLS", COLS)), int(os.environ.get("TARAE_SHOT_ROWS", ROWS))
else:
    fcntl.ioctl(m, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
screen = pyte.Screen(COLS, ROWS); stream = pyte.ByteStream(screen)
def pump(t):
    end = time.time() + t
    while time.time() < end:
        r, _, _ = select.select([m], [], [], 0.02)
        if r:
            try: stream.feed(os.read(m, 65536))
            except OSError: return
if REPLAY:
    stream.feed(open(REPLAY, "rb").read()); keys = []
else:
    pump(1.2)
for k in keys:
    if k.startswith("@"):          # @3 = wait 3 s taking screen updates (language server indexing etc.)
        pump(float(k[1:])); continue
    # Only unescape escapes like \\r·\\x1b; everything else (Korean etc.) is sent as UTF-8 as is
    k = re.sub(r"\\x([0-9a-fA-F]{2})", lambda m: chr(int(m.group(1), 16)), k.replace("\\r", "\r").replace("\\t", "\t"))
    os.write(m, k.encode()); pump(0.35)
if not REPLAY:
    pump(0.6)

def find(pat):
    for d in [os.path.expanduser("~/Library/Fonts"), "/Library/Fonts", "/System/Library/Fonts"]:
        g = glob.glob(os.path.join(d, pat))
        if g: return g[0]
SIZE = 15
mono = ImageFont.truetype(find("JetBrainsMono-Regular.ttf"), SIZE)
mono_b = ImageFont.truetype(find("JetBrainsMono-Bold.ttf") or find("JetBrainsMono-Regular.ttf"), SIZE)
mono_i = ImageFont.truetype(find("JetBrainsMono-Italic.ttf") or find("JetBrainsMono-Regular.ttf"), SIZE)
hangul = ImageFont.truetype("/System/Library/Fonts/AppleSDGothicNeo.ttc", SIZE)
cw = int(mono.getlength("M")); ch = int(SIZE * 1.45)
BG, FG = (30, 31, 40), (248, 248, 242)
names = {"black":(33,34,44),"red":(255,85,85),"green":(80,250,123),"brown":(241,250,140),"yellow":(241,250,140),
         "blue":(189,147,249),"magenta":(255,121,198),"cyan":(139,233,253),"white":(248,248,242)}
def col(c, default):
    if c == "default": return default
    if isinstance(c, str) and len(c) == 6:
        try: return tuple(int(c[i:i+2], 16) for i in (0, 2, 4))
        except ValueError: pass
    return names.get(str(c).replace("bright", ""), default)
PAD = 14
# The outer margin uses the edit view's background (rightmost cell of the command line — a spot no picker·card covers)
BG = col(screen.buffer[ROWS - 1][COLS - 1].bg, BG)
img = Image.new("RGB", (COLS*cw + PAD*2, ROWS*ch + PAD*2), BG)
d = ImageDraw.Draw(img)
# Line chars: as lines through the cell center (filling the cell to connect, like a real terminal)
BOX = {"─":"lr","│":"ud","╭":"rd","╮":"ld","╰":"ru","╯":"lu","├":"udr","┤":"udl","┬":"lrd","┴":"lru","┼":"udlr","┌":"rd","┐":"ld","└":"ru","┘":"lu"}
def cells():
    for y in range(ROWS):
        row = screen.buffer[y]
        for x in range(COLS):
            c = row[x]
            fg, bg = col(c.fg, FG), col(c.bg, BG)
            if c.reverse: fg, bg = bg, fg
            wide = bool(c.data) and x + 1 < COLS and row[x+1].data == "" and ord(c.data[0]) > 0x1100
            yield x, y, c, fg, bg, wide
# 1) All backgrounds first — so wide glyphs aren't half-covered by the neighbor cell's background
for x, y, c, fg, bg, wide in cells():
    if bg != BG:
        px, py = PAD + x*cw, PAD + y*ch
        d.rectangle([px, py, px + cw*(2 if wide else 1) - 1, py + ch - 1], fill=bg)
# 2) Glyphs
for x, y, c, fg, bg, wide in cells():
    if not c.data or not c.data.strip():
        continue
    px, py = PAD + x*cw, PAD + y*ch
    # Block chars: as rectangles exactly filling the cell (as a terminal draws them — font glyphs leave gaps)
    BLOCKS = {"▀": (0, 0, 1, .5), "▄": (0, .5, 1, 1), "▌": (0, 0, .5, 1), "▐": (.5, 0, 1, 1),
              "█": (0, 0, 1, 1), "▎": (0, 0, .25, 1), "▏": (0, 0, .125, 1)}
    if c.data in BLOCKS:
        a0, b0, a1, b1 = BLOCKS[c.data]
        d.rectangle([px + round(a0*cw), py + round(b0*ch), px + round(a1*cw) - 1, py + round(b1*ch) - 1], fill=fg)
        continue
    if c.data in BOX:
        mx, my = px + cw//2, py + ch//2
        for dd in BOX[c.data]:
            if dd == "l": d.line([px, my, mx, my], fill=fg, width=1)
            if dd == "r": d.line([mx, my, px + cw, my], fill=fg, width=1)
            if dd == "u": d.line([mx, py, mx, my], fill=fg, width=1)
            if dd == "d": d.line([mx, my, mx, py + ch], fill=fg, width=1)
        continue
    f = hangul if ord(c.data[0]) > 0x2e80 else (mono_b if c.bold else mono_i if c.italics else mono)
    d.text((px, py + 3), c.data, font=f, fill=fg)
    if c.underscore:
        w = cw * (2 if wide else 1)
        d.line([px, py + ch - 3, px + w - 1, py + ch - 3], fill=fg, width=1)
cx, cy = screen.cursor.x, screen.cursor.y
if not screen.cursor.hidden:
    cur_fg = col(screen.buffer[cy][cx].fg, FG) if cx < COLS else FG
    d.rectangle([PAD + cx*cw, PAD + cy*ch, PAD + cx*cw + 1, PAD + cy*ch + ch - 1], fill=cur_fg)
img.save(out_png)
if not REPLAY:
    os.write(m, b"\x1b:q!\r"); time.sleep(0.3)
print("saved", out_png, img.size)
