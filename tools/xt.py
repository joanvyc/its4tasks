"""Drive the its4tasks X window: xt.py id | move X Y | click X Y [button] | drag X1 Y1 X2 Y2 | type TEXT | key NAME [shift] | shot PATH

Coordinates are relative to the window (same as a `shot` image).

Drives the window named exactly "its4tasks" (the native GUI), or with
XT_WINDOW set, the first window whose name starts with it, e.g.
XT_WINDOW="its4tasks — Mozilla Firefox" for the web GUI.
"""
import ctypes as C
import os
import subprocess
import sys
import time

x11 = C.CDLL("libX11.so.6")
xt = C.CDLL("libXtst.so.6")
x11.XOpenDisplay.restype = C.c_void_p
x11.XDefaultRootWindow.restype = C.c_ulong
x11.XDefaultRootWindow.argtypes = [C.c_void_p]
x11.XQueryTree.argtypes = [C.c_void_p, C.c_ulong, C.POINTER(C.c_ulong), C.POINTER(C.c_ulong),
                           C.POINTER(C.POINTER(C.c_ulong)), C.POINTER(C.c_uint)]
x11.XFetchName.argtypes = [C.c_void_p, C.c_ulong, C.POINTER(C.c_char_p)]
x11.XTranslateCoordinates.argtypes = [C.c_void_p, C.c_ulong, C.c_ulong, C.c_int, C.c_int,
                                      C.POINTER(C.c_int), C.POINTER(C.c_int), C.POINTER(C.c_ulong)]
x11.XStringToKeysym.restype = C.c_ulong
x11.XStringToKeysym.argtypes = [C.c_char_p]
x11.XKeysymToKeycode.argtypes = [C.c_void_p, C.c_ulong]
x11.XFlush.argtypes = [C.c_void_p]
xt.XTestFakeMotionEvent.argtypes = [C.c_void_p, C.c_int, C.c_int, C.c_int, C.c_ulong]
xt.XTestFakeButtonEvent.argtypes = [C.c_void_p, C.c_uint, C.c_int, C.c_ulong]
xt.XTestFakeKeyEvent.argtypes = [C.c_void_p, C.c_uint, C.c_int, C.c_ulong]

d = x11.XOpenDisplay(None)
root = x11.XDefaultRootWindow(d)


x11.XInternAtom.restype = C.c_ulong
x11.XInternAtom.argtypes = [C.c_void_p, C.c_char_p, C.c_int]
x11.XGetWindowProperty.argtypes = [C.c_void_p, C.c_ulong, C.c_ulong, C.c_long, C.c_long, C.c_int,
                                   C.c_ulong, C.POINTER(C.c_ulong), C.POINTER(C.c_int),
                                   C.POINTER(C.c_ulong), C.POINTER(C.c_ulong),
                                   C.POINTER(C.c_char_p)]


def title(win):
    """The window's UTF-8 _NET_WM_NAME (Firefox only sets that and a
    COMPOUND_TEXT WM_NAME, which XFetchName can't read), else its WM_NAME."""
    kind, fmt, count, after, data = C.c_ulong(), C.c_int(), C.c_ulong(), C.c_ulong(), C.c_char_p()
    net_wm_name = x11.XInternAtom(d, b"_NET_WM_NAME", 0)
    utf8 = x11.XInternAtom(d, b"UTF8_STRING", 0)
    if (x11.XGetWindowProperty(d, win, net_wm_name, 0, 1024, 0, utf8, C.byref(kind), C.byref(fmt),
                               C.byref(count), C.byref(after), C.byref(data)) == 0
            and data.value):
        return data.value
    n = C.c_char_p()
    if x11.XFetchName(d, win, C.byref(n)):
        return n.value
    return None


def matches(t):
    return t is not None and (t.startswith(name) if prefix else t == name)


def find(win):
    if matches(title(win)):
        return win
    r, p, kids, cnt = C.c_ulong(), C.c_ulong(), C.POINTER(C.c_ulong)(), C.c_uint()
    if not x11.XQueryTree(d, win, C.byref(r), C.byref(p), C.byref(kids), C.byref(cnt)):
        return None
    for i in range(cnt.value):
        w = find(kids[i])
        if w:
            return w
    return None


prefix = "XT_WINDOW" in os.environ
name = os.environ.get("XT_WINDOW", "its4tasks").encode()
win = find(root)
if not win:
    sys.exit(f"{name.decode()} window not found")


def origin():
    x, y, child = C.c_int(), C.c_int(), C.c_ulong()
    x11.XTranslateCoordinates(d, win, root, 0, 0, C.byref(x), C.byref(y), C.byref(child))
    return x.value, y.value


def move(x, y):
    ox, oy = origin()
    xt.XTestFakeMotionEvent(d, -1, ox + x, oy + y, 0)
    x11.XFlush(d)
    time.sleep(0.15)


def key(name, shift=False):
    shift_code = x11.XKeysymToKeycode(d, x11.XStringToKeysym(b"Shift_L"))
    code = x11.XKeysymToKeycode(d, x11.XStringToKeysym(name.encode()))
    if shift:
        xt.XTestFakeKeyEvent(d, shift_code, 1, 0)
    xt.XTestFakeKeyEvent(d, code, 1, 0)
    xt.XTestFakeKeyEvent(d, code, 0, 0)
    if shift:
        xt.XTestFakeKeyEvent(d, shift_code, 0, 0)
    x11.XFlush(d)
    time.sleep(0.03)


SPECIAL = {" ": "space", ",": "comma", ".": "period", "'": "apostrophe", "-": "minus", "/": "slash"}
# Typed with Shift held (on a US layout).
SHIFTED = {":": "colon", "_": "underscore", "?": "question", "!": "exclam"}

cmd, args = sys.argv[1], sys.argv[2:]
if cmd == "id":
    print(hex(win), origin())
elif cmd == "move":
    move(int(args[0]), int(args[1]))
elif cmd == "click":
    move(int(args[0]), int(args[1]))
    b = int(args[2]) if len(args) > 2 else 1
    xt.XTestFakeButtonEvent(d, b, 1, 0)
    x11.XFlush(d)
    time.sleep(0.05)
    xt.XTestFakeButtonEvent(d, b, 0, 0)
    x11.XFlush(d)
    time.sleep(0.3)
elif cmd == "drag":
    x1, y1, x2, y2 = map(int, args[:4])
    move(x1, y1)
    xt.XTestFakeButtonEvent(d, 1, 1, 0)
    x11.XFlush(d)
    time.sleep(0.1)
    # In steps, so egui sees the drag start and the hover over the target.
    for i in range(1, 11):
        move(x1 + (x2 - x1) * i // 10, y1 + (y2 - y1) * i // 10)
    time.sleep(0.2)
    xt.XTestFakeButtonEvent(d, 1, 0, 0)
    x11.XFlush(d)
    time.sleep(0.3)
elif cmd == "type":
    for ch in args[0]:
        if ch in SHIFTED:
            key(SHIFTED[ch], shift=True)
        else:
            key(SPECIAL.get(ch, ch.lower()), shift=ch.isupper())
    time.sleep(0.2)
elif cmd == "key":
    key(args[0], shift=len(args) > 1 and args[1] == "shift")
    time.sleep(0.3)
elif cmd == "shot":
    time.sleep(0.4)
    subprocess.run(["import", "-window", hex(win), args[0]], check=True)
