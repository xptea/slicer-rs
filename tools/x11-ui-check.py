#!/usr/bin/env python3
"""Small local X11 interaction helper for Slicer's native acceptance checks."""
import ctypes as c
import sys
x = c.CDLL('libX11.so.6')
t = c.CDLL('libXtst.so.6')
x.XOpenDisplay.restype = c.c_void_p
x.XOpenDisplay.argtypes = [c.c_char_p]
d = x.XOpenDisplay(None)
if not d:
    raise SystemExit('No X11 display connection')
x.XRaiseWindow.argtypes = [c.c_void_p, c.c_ulong]
x.XSetInputFocus.argtypes = [c.c_void_p, c.c_ulong, c.c_int, c.c_ulong]
x.XWarpPointer.argtypes = [c.c_void_p, c.c_ulong, c.c_ulong, c.c_int, c.c_int, c.c_uint, c.c_uint, c.c_int, c.c_int]
x.XFlush.argtypes = [c.c_void_p]
x.XCloseDisplay.argtypes = [c.c_void_p]
x.XStringToKeysym.argtypes = [c.c_char_p]
x.XStringToKeysym.restype = c.c_ulong
x.XKeysymToKeycode.argtypes = [c.c_void_p, c.c_ulong]
x.XKeysymToKeycode.restype = c.c_uint
t.XTestFakeButtonEvent.argtypes = [c.c_void_p, c.c_uint, c.c_int, c.c_ulong]
t.XTestFakeKeyEvent.argtypes = [c.c_void_p, c.c_uint, c.c_int, c.c_ulong]
w = int(sys.argv[1], 0)
x.XRaiseWindow(d, w)
x.XSetInputFocus(d, w, 1, 0)
if sys.argv[2] == 'click':
    x.XWarpPointer(d, 0, w, 0, 0, 0, 0, int(sys.argv[3]), int(sys.argv[4]))
    t.XTestFakeButtonEvent(d, 1, 1, 0)
    t.XTestFakeButtonEvent(d, 1, 0, 0)
elif sys.argv[2] == 'drag':
    import time
    start_x, start_y, end_x, end_y = map(int, sys.argv[3:7])
    x.XWarpPointer(d, 0, w, 0, 0, 0, 0, start_x, start_y)
    t.XTestFakeButtonEvent(d, 1, 1, 0)
    x.XFlush(d)
    time.sleep(0.05)
    for step in range(1, 11):
        x.XWarpPointer(d, 0, w, 0, 0, 0, 0,
                       start_x + (end_x-start_x)*step//10,
                       start_y + (end_y-start_y)*step//10)
        x.XFlush(d)
        time.sleep(0.03)
    t.XTestFakeButtonEvent(d, 1, 0, 0)
elif sys.argv[2] == 'key':
    keys = [x.XKeysymToKeycode(d, x.XStringToKeysym(k.encode())) for k in sys.argv[3:]]
    for key in keys:
        t.XTestFakeKeyEvent(d, key, 1, 0)
    for key in reversed(keys):
        t.XTestFakeKeyEvent(d, key, 0, 0)
elif sys.argv[2] == 'scroll':
    for _ in range(int(sys.argv[3])):
        t.XTestFakeButtonEvent(d, 5, 1, 0)
        t.XTestFakeButtonEvent(d, 5, 0, 0)
x.XFlush(d)
x.XCloseDisplay(d)
