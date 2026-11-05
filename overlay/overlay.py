"""Jcode agent overlay for Windows.

A transparent, always-on-top, click-through layer that shows the user what the
agent is doing on the desktop:

* a glowing purple frame around every monitor (pulses while the agent works),
* a separate purple "agent cursor" that glides to the point the agent acts on
  and marks clicks with a ripple,
* a small "stop" pill at the top of the primary monitor; a real (non-injected)
  click on it, or a real Esc key press, calls ``on_stop``.

Everything is plain Win32 through ctypes: per-pixel-alpha layered windows
(``UpdateLayeredWindow``) with WS_EX_TRANSPARENT so the overlay never takes
clicks or focus.  Images are drawn with Pillow + numpy.  All windows live on
one GUI thread; other threads talk to it through ``Overlay`` methods, which are
thread-safe.
"""

from __future__ import annotations

import collections
import ctypes
import math
import os
import threading
import time
from ctypes import wintypes

import numpy as np
from PIL import Image, ImageDraw, ImageFilter, ImageFont

user32 = ctypes.WinDLL("user32", use_last_error=True)
gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

# ---------------------------------------------------------------- constants
WS_POPUP = 0x80000000
WS_EX_LAYERED = 0x00080000
WS_EX_TRANSPARENT = 0x00000020
WS_EX_TOPMOST = 0x00000008
WS_EX_TOOLWINDOW = 0x00000080
WS_EX_NOACTIVATE = 0x08000000
ULW_ALPHA = 0x2
AC_SRC_OVER = 0
AC_SRC_ALPHA = 1
SW_HIDE = 0
SW_SHOWNOACTIVATE = 4
SWP_NOSIZE = 0x1
SWP_NOMOVE = 0x2
SWP_NOACTIVATE = 0x10
SWP_SHOWWINDOW = 0x40
HWND_TOPMOST = wintypes.HWND(-1)
HWND_MESSAGE = wintypes.HWND(-3)
WM_TIMER = 0x0113
WM_APP = 0x8000
WM_KEYDOWN = 0x0100
WM_SYSKEYDOWN = 0x0104
WM_LBUTTONDOWN = 0x0201
WM_LBUTTONUP = 0x0202
WH_KEYBOARD_LL = 13
WH_MOUSE_LL = 14
LLKHF_INJECTED = 0x10
LLMHF_INJECTED = 0x1
VK_ESCAPE = 0x1B
DIB_RGB_COLORS = 0
BI_RGB = 0

PURPLE = (168, 85, 247)       # #A855F7
VIOLET = (124, 58, 237)       # #7C3AED
FUCHSIA = (217, 70, 239)      # #D946EF
LILAC = (233, 213, 255)       # #E9D5FF

LRESULT = ctypes.c_ssize_t
WNDPROC = ctypes.WINFUNCTYPE(LRESULT, wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM)
HOOKPROC = ctypes.WINFUNCTYPE(LRESULT, ctypes.c_int, wintypes.WPARAM, wintypes.LPARAM)
MONITORENUMPROC = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HMONITOR, wintypes.HDC, ctypes.POINTER(wintypes.RECT), wintypes.LPARAM)


class WNDCLASSEXW(ctypes.Structure):
    _fields_ = [("cbSize", wintypes.UINT), ("style", wintypes.UINT), ("lpfnWndProc", WNDPROC),
                ("cbClsExtra", ctypes.c_int), ("cbWndExtra", ctypes.c_int), ("hInstance", wintypes.HINSTANCE),
                ("hIcon", wintypes.HICON), ("hCursor", wintypes.HANDLE), ("hbrBackground", wintypes.HBRUSH),
                ("lpszMenuName", wintypes.LPCWSTR), ("lpszClassName", wintypes.LPCWSTR), ("hIconSm", wintypes.HICON)]


class BLENDFUNCTION(ctypes.Structure):
    _fields_ = [("BlendOp", ctypes.c_ubyte), ("BlendFlags", ctypes.c_ubyte),
                ("SourceConstantAlpha", ctypes.c_ubyte), ("AlphaFormat", ctypes.c_ubyte)]


class BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [("biSize", wintypes.DWORD), ("biWidth", ctypes.c_long), ("biHeight", ctypes.c_long),
                ("biPlanes", wintypes.WORD), ("biBitCount", wintypes.WORD), ("biCompression", wintypes.DWORD),
                ("biSizeImage", wintypes.DWORD), ("biXPelsPerMeter", ctypes.c_long), ("biYPelsPerMeter", ctypes.c_long),
                ("biClrUsed", wintypes.DWORD), ("biClrImportant", wintypes.DWORD)]


class MONITORINFO(ctypes.Structure):
    _fields_ = [("cbSize", wintypes.DWORD), ("rcMonitor", wintypes.RECT), ("rcWork", wintypes.RECT), ("dwFlags", wintypes.DWORD)]


class KBDLLHOOKSTRUCT(ctypes.Structure):
    _fields_ = [("vkCode", wintypes.DWORD), ("scanCode", wintypes.DWORD), ("flags", wintypes.DWORD),
                ("time", wintypes.DWORD), ("dwExtraInfo", ctypes.c_size_t)]


class MSLLHOOKSTRUCT(ctypes.Structure):
    _fields_ = [("pt", wintypes.POINT), ("mouseData", wintypes.DWORD), ("flags", wintypes.DWORD),
                ("time", wintypes.DWORD), ("dwExtraInfo", ctypes.c_size_t)]


user32.DefWindowProcW.restype = LRESULT
user32.DefWindowProcW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
user32.CallNextHookEx.restype = LRESULT
user32.CallNextHookEx.argtypes = [wintypes.HHOOK, ctypes.c_int, wintypes.WPARAM, wintypes.LPARAM]
user32.SetWindowsHookExW.restype = wintypes.HHOOK
user32.SetWindowsHookExW.argtypes = [ctypes.c_int, HOOKPROC, wintypes.HINSTANCE, wintypes.DWORD]
user32.CreateWindowExW.restype = wintypes.HWND
user32.CreateWindowExW.argtypes = [wintypes.DWORD, wintypes.LPCWSTR, wintypes.LPCWSTR, wintypes.DWORD, ctypes.c_int, ctypes.c_int,
                                   ctypes.c_int, ctypes.c_int, wintypes.HWND, wintypes.HMENU, wintypes.HINSTANCE, wintypes.LPVOID]
user32.UpdateLayeredWindow.argtypes = [wintypes.HWND, wintypes.HDC, ctypes.POINTER(wintypes.POINT), ctypes.POINTER(wintypes.SIZE),
                                       wintypes.HDC, ctypes.POINTER(wintypes.POINT), wintypes.COLORREF,
                                       ctypes.POINTER(BLENDFUNCTION), wintypes.DWORD]
user32.SetWindowPos.argtypes = [wintypes.HWND, wintypes.HWND, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int, wintypes.UINT]
user32.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
user32.SetTimer.argtypes = [wintypes.HWND, ctypes.c_size_t, wintypes.UINT, wintypes.LPVOID]
user32.SetTimer.restype = ctypes.c_size_t
user32.KillTimer.argtypes = [wintypes.HWND, ctypes.c_size_t]
gdi32.CreateDIBSection.restype = wintypes.HBITMAP
gdi32.CreateDIBSection.argtypes = [wintypes.HDC, ctypes.c_void_p, wintypes.UINT, ctypes.POINTER(ctypes.c_void_p), wintypes.HANDLE, wintypes.DWORD]
gdi32.SelectObject.restype = wintypes.HGDIOBJ
gdi32.SelectObject.argtypes = [wintypes.HDC, wintypes.HGDIOBJ]
gdi32.CreateCompatibleDC.restype = wintypes.HDC
gdi32.CreateCompatibleDC.argtypes = [wintypes.HDC]
gdi32.DeleteObject.argtypes = [wintypes.HGDIOBJ]
gdi32.DeleteDC.argtypes = [wintypes.HDC]
gdi32.CreateFontW.restype = wintypes.HFONT
gdi32.CreateFontW.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int, wintypes.DWORD, wintypes.DWORD,
                              wintypes.DWORD, wintypes.DWORD, wintypes.DWORD, wintypes.DWORD, wintypes.DWORD, wintypes.DWORD, wintypes.LPCWSTR]
gdi32.SetBkMode.argtypes = [wintypes.HDC, ctypes.c_int]
gdi32.SetTextColor.argtypes = [wintypes.HDC, wintypes.COLORREF]
user32.GetDC.restype = wintypes.HDC
user32.GetDC.argtypes = [wintypes.HWND]
user32.ShowWindow.argtypes = [wintypes.HWND, ctypes.c_int]
user32.UnhookWindowsHookEx.argtypes = [wintypes.HHOOK]
user32.GetMonitorInfoW.argtypes = [wintypes.HMONITOR, ctypes.c_void_p]
user32.EnumDisplayMonitors.argtypes = [wintypes.HDC, ctypes.c_void_p, MONITORENUMPROC, wintypes.LPARAM]
user32.RegisterClassExW.argtypes = [ctypes.c_void_p]
user32.GetMessageW.argtypes = [ctypes.c_void_p, wintypes.HWND, wintypes.UINT, wintypes.UINT]
user32.TranslateMessage.argtypes = [ctypes.c_void_p]
user32.DispatchMessageW.argtypes = [ctypes.c_void_p]
user32.DispatchMessageW.restype = LRESULT
user32.DrawTextW.argtypes = [wintypes.HDC, wintypes.LPCWSTR, ctypes.c_int, ctypes.POINTER(wintypes.RECT), wintypes.UINT]
kernel32.GetModuleHandleW.restype = wintypes.HMODULE


def set_dpi_awareness() -> None:
    try:
        user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))  # PER_MONITOR_AWARE_V2
    except Exception:
        try:
            ctypes.windll.shcore.SetProcessDpiAwareness(2)
        except Exception:
            pass


def monitors() -> list[tuple[int, int, int, int, bool]]:
    """(x, y, w, h, primary) of every monitor in physical pixels."""
    out: list[tuple[int, int, int, int, bool]] = []

    def cb(hmon, _hdc, _rect, _lp):
        mi = MONITORINFO()
        mi.cbSize = ctypes.sizeof(MONITORINFO)
        user32.GetMonitorInfoW(hmon, ctypes.byref(mi))
        r = mi.rcMonitor
        out.append((r.left, r.top, r.right - r.left, r.bottom - r.top, bool(mi.dwFlags & 1)))
        return True

    proc = MONITORENUMPROC(cb)
    user32.EnumDisplayMonitors(None, None, proc, 0)
    return out or [(0, 0, user32.GetSystemMetrics(0), user32.GetSystemMetrics(1), True)]


def cursor_pos() -> tuple[int, int]:
    pt = wintypes.POINT()
    user32.GetCursorPos(ctypes.byref(pt))
    return pt.x, pt.y


# ---------------------------------------------------------------- imaging
def to_premultiplied_bgra(img: Image.Image) -> bytes:
    a = np.asarray(img.convert("RGBA"), dtype=np.uint16)
    alpha = a[..., 3:4]
    rgb = (a[..., :3] * alpha + 127) // 255
    out = np.empty(a.shape, dtype=np.uint8)
    out[..., 0] = rgb[..., 2]
    out[..., 1] = rgb[..., 1]
    out[..., 2] = rgb[..., 0]
    out[..., 3] = alpha[..., 0]
    return out.tobytes()


def make_frame_image(w: int, h: int, thickness: int = 34) -> Image.Image:
    """Glowing purple frame for a w x h monitor (RGBA, mostly transparent)."""
    ys, xs = np.mgrid[0:h, 0:w]
    d = np.minimum(np.minimum(xs, ys), np.minimum(w - 1 - xs, h - 1 - ys)).astype(np.float32)
    core = np.clip(1.0 - (d - 2.5) / 1.5, 0, 1)                 # crisp 3-4px line at the edge
    glow = np.exp(-d / 9.0) * 0.85                              # soft falloff inwards
    alpha = np.clip(np.maximum(core, glow), 0, 1)
    alpha[d > thickness] = 0
    # colour drifts along the perimeter: violet -> fuchsia -> purple
    t = (np.arctan2(ys - h / 2, xs - w / 2) / (2 * math.pi) + 0.5).astype(np.float32)
    wave = (np.sin(t * 2 * math.pi * 2) + 1) / 2
    c0 = np.array(VIOLET, np.float32)
    c1 = np.array(FUCHSIA, np.float32)
    col = c0[None, None, :] * (1 - wave[..., None]) + c1[None, None, :] * wave[..., None]
    lil = np.array(LILAC, np.float32)
    col = col * (1 - core[..., None] * 0.7) + lil[None, None, :] * (core[..., None] * 0.7)
    rgba = np.zeros((h, w, 4), np.uint8)
    rgba[..., :3] = np.clip(col, 0, 255).astype(np.uint8)
    rgba[..., 3] = (alpha * 255).astype(np.uint8)
    return Image.fromarray(rgba, "RGBA")


CURSOR_SIZE = 168
HOT = CURSOR_SIZE // 2


def _font(size: int, bold: bool = True):
    for name in (("segoeuib.ttf" if bold else "segoeui.ttf"), "tahomabd.ttf", "arial.ttf"):
        try:
            return ImageFont.truetype(os.path.join(os.environ.get("WINDIR", r"C:\Windows"), "Fonts", name), size)
        except OSError:
            continue
    return ImageFont.load_default()


def make_cursor_image(label: str = "Jcode", ripple: float | None = None, typing_phase: float | None = None) -> Image.Image:
    s = CURSOR_SIZE
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    if ripple is not None:
        # expanding double ring
        r_img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
        dr = ImageDraw.Draw(r_img)
        for k, delay in ((0, 0.0), (1, 0.25)):
            p = (ripple - delay) / (1 - delay) if ripple >= delay else None
            if p is None or p > 1:
                continue
            r = 6 + p * 62
            a = int(230 * (1 - p) ** 1.4)
            w = max(2, int(6 * (1 - p)) + 2)
            dr.ellipse([HOT - r, HOT - r, HOT + r, HOT + r], outline=PURPLE + (a,), width=w)
        if ripple < 0.35:
            q = ripple / 0.35
            r = 10 + 8 * q
            dr.ellipse([HOT - r, HOT - r, HOT + r, HOT + r], fill=FUCHSIA + (int(150 * (1 - q)),))
        img.alpha_composite(r_img.filter(ImageFilter.GaussianBlur(0.6)))
    # arrow pointer, tip at the hotspot
    sc = 1.5
    pts = [(0, 0), (0, 17), (4.2, 13.2), (7.2, 20), (9.8, 18.9), (6.9, 12.2), (12.3, 12.2)]
    poly = [(HOT + x * sc, HOT + y * sc) for x, y in pts]
    glow = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    ImageDraw.Draw(glow).polygon(poly, fill=PURPLE + (200,))
    img.alpha_composite(glow.filter(ImageFilter.GaussianBlur(5)))
    d = ImageDraw.Draw(img)
    d.polygon(poly, fill=(147, 51, 234, 255), outline=(255, 255, 255, 255), width=2)
    # name tag
    f = _font(13)
    text = label
    if typing_phase is not None:
        dots = int(typing_phase * 3) % 3 + 1
        text = f"{label} · typing" + "." * dots
    tw = d.textlength(text, font=f)
    x0, y0 = HOT + 16, HOT + 30
    d.rounded_rectangle([x0, y0, x0 + tw + 16, y0 + 22], radius=11, fill=(124, 58, 237, 235), outline=(233, 213, 255, 200))
    d.text((x0 + 8, y0 + 2), text, font=f, fill=(255, 255, 255, 255))
    return img


def gdi_text_mask(text: str, w: int, h: int, size: int = 15, bold: bool = True) -> np.ndarray:
    """Render (Persian-capable, shaped, RTL) text with GDI; return an alpha mask."""
    hdc = gdi32.CreateCompatibleDC(None)
    bmi = BITMAPINFOHEADER()
    bmi.biSize = ctypes.sizeof(BITMAPINFOHEADER)
    bmi.biWidth, bmi.biHeight, bmi.biPlanes, bmi.biBitCount, bmi.biCompression = w, -h, 1, 32, BI_RGB
    bits = ctypes.c_void_p()
    hbmp = gdi32.CreateDIBSection(hdc, ctypes.byref(bmi), DIB_RGB_COLORS, ctypes.byref(bits), None, 0)
    old = gdi32.SelectObject(hdc, hbmp)
    font = gdi32.CreateFontW(-size, 0, 0, 0, 700 if bold else 400, 0, 0, 0, 1, 0, 0, 4, 0, "Segoe UI")
    oldf = gdi32.SelectObject(hdc, font)
    gdi32.SetBkMode(hdc, 1)
    gdi32.SetTextColor(hdc, 0x00FFFFFF)
    rc = wintypes.RECT(0, 0, w, h)
    DT_CENTER, DT_VCENTER, DT_SINGLELINE, DT_RTLREADING = 0x1, 0x4, 0x20, 0x20000
    user32.DrawTextW(hdc, text, -1, ctypes.byref(rc), DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_RTLREADING)
    gdi32.GdiFlush()
    buf = (ctypes.c_ubyte * (w * h * 4)).from_address(bits.value)
    arr = np.frombuffer(buf, dtype=np.uint8).reshape(h, w, 4)[..., 1].copy()
    gdi32.SelectObject(hdc, oldf)
    gdi32.DeleteObject(font)
    gdi32.SelectObject(hdc, old)
    gdi32.DeleteObject(hbmp)
    gdi32.DeleteDC(hdc)
    return arr


STOP_W, STOP_H = 290, 40


def make_stop_image(hover: bool = False) -> Image.Image:
    img = Image.new("RGBA", (STOP_W, STOP_H), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    bg = (109, 40, 217, 245) if hover else (88, 28, 135, 235)
    d.rounded_rectangle([1, 1, STOP_W - 2, STOP_H - 2], radius=STOP_H // 2 - 1, fill=bg, outline=(216, 180, 254, 255), width=2)
    # stop square on the left
    d.rounded_rectangle([16, 13, 30, 27], radius=3, fill=(255, 255, 255, 255))
    mask = gdi_text_mask("توقف ایجنت Jcode  ·  Esc", STOP_W - 44, STOP_H, size=14)
    txt = Image.new("RGBA", (STOP_W - 44, STOP_H), (255, 255, 255, 0))
    txt.putalpha(Image.fromarray(mask, "L"))
    img.alpha_composite(txt, (38, 0))
    return img


# ---------------------------------------------------------------- windows
class LayeredWindow:
    def __init__(self, cls_name: str, hinst, x: int, y: int, w: int, h: int, owner=None):
        self.x, self.y, self.w, self.h = x, y, w, h
        ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE
        self.hwnd = user32.CreateWindowExW(ex, cls_name, "Jcode agent overlay", WS_POPUP, x, y, w, h, None, None, hinst, None)
        self.hdc_screen = user32.GetDC(None)
        self.memdc = gdi32.CreateCompatibleDC(self.hdc_screen)
        bmi = BITMAPINFOHEADER()
        bmi.biSize = ctypes.sizeof(BITMAPINFOHEADER)
        bmi.biWidth, bmi.biHeight, bmi.biPlanes, bmi.biBitCount, bmi.biCompression = w, -h, 1, 32, BI_RGB
        self.bits = ctypes.c_void_p()
        self.hbmp = gdi32.CreateDIBSection(self.memdc, ctypes.byref(bmi), DIB_RGB_COLORS, ctypes.byref(self.bits), None, 0)
        gdi32.SelectObject(self.memdc, self.hbmp)
        self.alpha = 0
        self.visible = False

    def set_image(self, bgra: bytes) -> None:
        ctypes.memmove(self.bits, bgra, len(bgra))
        self._update(full=True)

    def _update(self, full: bool) -> None:
        blend = BLENDFUNCTION(AC_SRC_OVER, 0, max(0, min(255, int(self.alpha))), AC_SRC_ALPHA)
        if full:
            pt_dst = wintypes.POINT(self.x, self.y)
            size = wintypes.SIZE(self.w, self.h)
            pt_src = wintypes.POINT(0, 0)
            user32.UpdateLayeredWindow(self.hwnd, self.hdc_screen, ctypes.byref(pt_dst), ctypes.byref(size),
                                       self.memdc, ctypes.byref(pt_src), 0, ctypes.byref(blend), ULW_ALPHA)
        else:
            user32.UpdateLayeredWindow(self.hwnd, None, None, None, None, None, 0, ctypes.byref(blend), ULW_ALPHA)

    def set_alpha(self, a: float) -> None:
        a = int(max(0, min(255, a)))
        if a == self.alpha:
            return
        self.alpha = a
        self._update(full=False)

    def move(self, x: int, y: int) -> None:
        self.x, self.y = x, y
        user32.SetWindowPos(self.hwnd, HWND_TOPMOST, x, y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE)

    def show(self) -> None:
        if not self.visible:
            self._update(full=True)
            user32.ShowWindow(self.hwnd, SW_SHOWNOACTIVATE)
            self.visible = True
        user32.SetWindowPos(self.hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE)

    def hide(self) -> None:
        if self.visible:
            user32.ShowWindow(self.hwnd, SW_HIDE)
            self.visible = False


CACHE_DIR = os.path.join(os.environ.get("LOCALAPPDATA", os.path.expanduser("~")), "JcodeOverlay", "cache")


def _cached(key: str, size: int, build) -> bytes:
    """Frame bitmaps are ~8 MB and take a second to render; keep them on disk."""
    path = os.path.join(CACHE_DIR, key + ".bgra")
    try:
        with open(path, "rb") as fh:
            data = fh.read()
        if len(data) == size:
            return data
    except OSError:
        pass
    data = build()
    try:
        os.makedirs(CACHE_DIR, exist_ok=True)
        with open(path + ".tmp", "wb") as fh:
            fh.write(data)
        os.replace(path + ".tmp", path)
    except OSError:
        pass
    return data


def _ease(t: float) -> float:
    return 1 - (1 - t) ** 3


class Overlay:
    """Thread-safe facade; the actual windows run on a private GUI thread."""

    IDLE_SECONDS = float(os.environ.get("JCODE_OVERLAY_IDLE", "4"))

    def __init__(self, on_stop=None):
        self.on_stop = on_stop
        self._cmds: collections.deque = collections.deque()
        self._ready = threading.Event()
        self._ctrl = None
        self._thread = threading.Thread(target=self._run, name="jcode-overlay", daemon=True)
        self._thread.start()
        self._ready.wait(10)

    # ---- public API (any thread) ----
    def activity(self) -> None:
        self._post(("activity",))

    def busy(self, delta: int) -> None:
        self._post(("busy", delta))

    def move_to(self, x: int, y: int) -> threading.Event:
        ev = threading.Event()
        self._post(("move", int(x), int(y), ev))
        return ev

    def click(self, x: int | None = None, y: int | None = None) -> None:
        self._post(("click", x, y))

    def typing(self, on: bool) -> None:
        self._post(("typing", on))

    def capture_hide(self, hidden: bool) -> threading.Event:
        ev = threading.Event()
        self._post(("capture", hidden, ev))
        return ev

    def flash_stopped(self) -> None:
        self._post(("stopped",))

    # ---- internals ----
    def _post(self, cmd) -> None:
        self._cmds.append(cmd)
        if self._ctrl:
            user32.PostMessageW(self._ctrl, WM_APP + 1, 0, 0)

    def _run(self) -> None:
        try:
            self._run_inner()
        except Exception as e:
            _log(f"overlay thread died: {e!r}")
            import traceback, sys
            traceback.print_exc(file=sys.stderr)
            self._ready.set()

    def _run_inner(self) -> None:
        set_dpi_awareness()
        hinst = kernel32.GetModuleHandleW(None)
        self._wndproc = WNDPROC(self._wnd_proc)
        wc = WNDCLASSEXW()
        wc.cbSize = ctypes.sizeof(WNDCLASSEXW)
        wc.lpfnWndProc = self._wndproc
        wc.hInstance = hinst
        wc.lpszClassName = "JcodeAgentOverlay"
        user32.RegisterClassExW(ctypes.byref(wc))
        self._ctrl = user32.CreateWindowExW(0, "JcodeAgentOverlay", "jcode-overlay-ctrl", 0, 0, 0, 0, 0, HWND_MESSAGE, None, hinst, None)

        self.frames: list[LayeredWindow] = []
        for (x, y, w, h, _p) in monitors():
            win = LayeredWindow("JcodeAgentOverlay", hinst, x, y, w, h)
            win.set_image(_cached(f"frame_v1_{w}x{h}", w * h * 4, lambda: to_premultiplied_bgra(make_frame_image(w, h))))
            self.frames.append(win)
        prim = next((m for m in monitors() if m[4]), monitors()[0])
        self.stop_rect = (prim[0] + (prim[2] - STOP_W) // 2, prim[1] + 10, STOP_W, STOP_H)
        self.stop = LayeredWindow("JcodeAgentOverlay", hinst, *self.stop_rect)
        self._stop_img = {False: to_premultiplied_bgra(make_stop_image(False)), True: to_premultiplied_bgra(make_stop_image(True))}
        self.stop.set_image(self._stop_img[False])
        self._stop_hover = False
        cx, cy = cursor_pos()
        self.cursor = LayeredWindow("JcodeAgentOverlay", hinst, cx - HOT, cy - HOT, CURSOR_SIZE, CURSOR_SIZE)
        self._cursor_base = to_premultiplied_bgra(make_cursor_image())
        self._ripples = [to_premultiplied_bgra(make_cursor_image(ripple=i / 17)) for i in range(18)]
        self._typing = [to_premultiplied_bgra(make_cursor_image(typing_phase=i / 3)) for i in range(3)]
        self.cursor.set_image(self._cursor_base)

        self.pos = (float(cx), float(cy))
        self.anim = None          # (x0, y0, x1, y1, t0, dur, event)
        self.ripple_t0 = None
        self.typing_on = False
        self.level = 0.0          # 0..1 global visibility
        self.target = 0.0
        self.busy_count = 0
        self.last_activity = 0.0
        self.capture_hidden = False
        self.stopped_flash_until = 0.0
        self._timer_on = False
        self._kb_hook = self._ms_hook = None
        self._kb_proc = HOOKPROC(self._kb_cb)
        self._ms_proc = HOOKPROC(self._ms_cb)
        self._ready.set()

        msg = wintypes.MSG()
        while user32.GetMessageW(ctypes.byref(msg), None, 0, 0) > 0:
            user32.TranslateMessage(ctypes.byref(msg))
            user32.DispatchMessageW(ctypes.byref(msg))

    def _wnd_proc(self, hwnd, msg, wp, lp):
        try:
            if msg == WM_APP + 1:
                self._drain()
                return 0
            if msg == WM_TIMER:
                self._tick()
                return 0
        except Exception as e:  # never let an exception cross the C boundary
            _log(f"overlay error: {e!r}")
        return user32.DefWindowProcW(hwnd, msg, wp, lp)

    def _ensure_timer(self) -> None:
        if not self._timer_on:
            self._last_tick = time.monotonic()
            user32.SetTimer(self._ctrl, 1, 16, None)
            self._timer_on = True

    def _wake(self) -> None:
        self.last_activity = time.monotonic()
        self.target = 1.0
        self._ensure_timer()
        self._install_hooks()

    def _drain(self) -> None:
        while self._cmds:
            cmd = self._cmds.popleft()
            kind = cmd[0]
            if kind == "activity":
                self._wake()
            elif kind == "busy":
                self.busy_count = max(0, self.busy_count + cmd[1])
                self._wake()
            elif kind == "move":
                _, x, y, ev = cmd
                self._wake()
                x0, y0 = self.pos
                if self.level < 0.05:      # appearing: start near the target, not across the screen
                    x0, y0 = x - 140, y - 90
                    self.pos = (x0, y0)
                dist = math.hypot(x - x0, y - y0)
                dur = 0.0 if dist < 2 else min(0.65, 0.22 + dist / 2600)
                if self.anim and self.anim[6] is not None:
                    self.anim[6].set()
                self.anim = (x0, y0, float(x), float(y), time.monotonic(), dur, ev)
            elif kind == "click":
                _, x, y = cmd
                self._wake()
                if x is not None:
                    if self.anim and self.anim[6] is not None:
                        self.anim[6].set()
                    self.anim = None
                    self.pos = (float(x), float(y))
                self.ripple_t0 = time.monotonic()
            elif kind == "typing":
                self.typing_on = cmd[1]
                self._wake()
            elif kind == "capture":
                _, hidden, ev = cmd
                self.capture_hidden = hidden
                self._apply_alpha()
                ev.set()
            elif kind == "stopped":
                self.busy_count = 0
                self.anim = None
                self.typing_on = False
                self.target = 0.0
                self.stopped_flash_until = time.monotonic() + 0.4
                self._ensure_timer()

    def _apply_alpha(self) -> None:
        lvl = 0.0 if self.capture_hidden else self.level
        pulse = 0.72 + 0.28 * math.sin(time.monotonic() * 2 * math.pi / 1.8)
        for f in self.frames:
            f.set_alpha(255 * lvl * pulse)
        self.cursor.set_alpha(255 * lvl)
        self.stop.set_alpha(255 * lvl)

    def _tick(self) -> None:
        now = time.monotonic()
        dt = min(0.1, now - getattr(self, "_last_tick", now))
        self._last_tick = now
        if self.busy_count == 0 and self.target > 0 and now - self.last_activity > self.IDLE_SECONDS:
            self.target = 0.0
        # time-based fade: 0.2 s in, 0.7 s out
        if self.level < self.target:
            self.level = min(self.target, self.level + dt / 0.2)
        elif self.level > self.target:
            self.level = max(self.target, self.level - dt / 0.7)
        if self.level > 0 and not self.cursor.visible:
            for f in self.frames:
                f.show()
            self.cursor.show()
            self.stop.show()
            _log("overlay shown")
        # keep the overlay above windows that became topmost later (every ~1 s)
        if self.level > 0 and now - getattr(self, "_last_raise", 0) > 1.0:
            self._last_raise = now
            for f in self.frames:
                f.show()
            self.stop.show()
            self.cursor.show()
        # cursor glide
        if self.anim:
            x0, y0, x1, y1, t0, dur, ev = self.anim
            p = 1.0 if dur <= 0 else min(1.0, (now - t0) / dur)
            e = _ease(p)
            self.pos = (x0 + (x1 - x0) * e, y0 + (y1 - y0) * e)
            if p >= 1.0:
                self.anim = None
                if ev is not None:
                    ev.set()
        cx, cy = int(self.pos[0]) - HOT, int(self.pos[1]) - HOT
        if (cx, cy) != (self.cursor.x, self.cursor.y):
            self.cursor.move(cx, cy)
        # cursor image: ripple > typing > base
        if self.ripple_t0 is not None:
            p = (now - self.ripple_t0) / 0.6
            if p >= 1:
                self.ripple_t0 = None
                self.cursor.set_image(self._cursor_base)
            else:
                self.cursor.set_image(self._ripples[min(17, int(p * 18))])
        elif self.typing_on:
            k = int(now * 3) % 3
            if k != getattr(self, "_typing_k", -1):
                self._typing_k = k
                self.cursor.set_image(self._typing[k])
        elif getattr(self, "_typing_k", -1) != -1:
            self._typing_k = -1
            self.cursor.set_image(self._cursor_base)
        self._apply_alpha()
        if self.level <= 0 and self.target <= 0 and not self.anim and self.ripple_t0 is None:
            for f in self.frames:
                f.hide()
            self.cursor.hide()
            self.stop.hide()
            self.cursor.set_image(self._cursor_base)
            user32.KillTimer(self._ctrl, 1)
            self._timer_on = False
            self._remove_hooks()
            _log("overlay hidden")

    # ---- stop: real Esc key or real click on the pill ----
    def _install_hooks(self) -> None:
        if self._kb_hook is None:
            self._kb_hook = user32.SetWindowsHookExW(WH_KEYBOARD_LL, self._kb_proc, kernel32.GetModuleHandleW(None), 0)
        if self._ms_hook is None:
            self._ms_hook = user32.SetWindowsHookExW(WH_MOUSE_LL, self._ms_proc, kernel32.GetModuleHandleW(None), 0)

    def _remove_hooks(self) -> None:
        if self._kb_hook:
            user32.UnhookWindowsHookEx(self._kb_hook)
            self._kb_hook = None
        if self._ms_hook:
            user32.UnhookWindowsHookEx(self._ms_hook)
            self._ms_hook = None

    def _fire_stop(self) -> None:
        cb = self.on_stop
        self._post(("stopped",))
        if cb:
            threading.Thread(target=cb, daemon=True).start()

    def _kb_cb(self, code, wp, lp):
        try:
            if code == 0 and wp in (WM_KEYDOWN, WM_SYSKEYDOWN) and self.level > 0.2:
                k = ctypes.cast(lp, ctypes.POINTER(KBDLLHOOKSTRUCT)).contents
                if k.vkCode == VK_ESCAPE and not (k.flags & LLKHF_INJECTED):
                    self._fire_stop()
        except Exception:
            pass
        return user32.CallNextHookEx(None, code, wp, lp)

    def _ms_cb(self, code, wp, lp):
        try:
            if code == 0 and self.level > 0.2:
                m = ctypes.cast(lp, ctypes.POINTER(MSLLHOOKSTRUCT)).contents
                x, y, w, h = self.stop_rect
                inside = x <= m.pt.x < x + w and y <= m.pt.y < y + h
                injected = bool(m.flags & LLMHF_INJECTED)
                if not injected and inside != self._stop_hover:
                    self._stop_hover = inside
                    self.stop.set_image(self._stop_img[inside])
                if inside and not injected and wp in (WM_LBUTTONDOWN, WM_LBUTTONUP):
                    if wp == WM_LBUTTONDOWN:
                        self._fire_stop()
                    return 1  # swallow the user's click on the pill
        except Exception:
            pass
        return user32.CallNextHookEx(None, code, wp, lp)


_LOG_PATH = os.environ.get("JCODE_OVERLAY_LOG")


def _log(text: str) -> None:
    if _LOG_PATH:
        try:
            with open(_LOG_PATH, "a", encoding="utf-8") as fh:
                fh.write(time.strftime("%H:%M:%S ") + text + "\n")
        except OSError:
            pass


if __name__ == "__main__":
    # Demo: python overlay.py  -> glides, clicks and types for a few seconds.
    stopped = threading.Event()
    ov = Overlay(on_stop=lambda: (print("STOP pressed"), stopped.set()))
    ov.busy(+1)
    for (x, y) in [(500, 400), (1200, 300), (900, 700)]:
        ov.move_to(x, y).wait(2)
        ov.click(x, y)
        time.sleep(0.8)
    ov.typing(True)
    time.sleep(2)
    ov.typing(False)
    ov.busy(-1)
    hold = float(os.environ.get("DEMO_HOLD", "6"))
    t_end = time.time() + hold
    while time.time() < t_end and not stopped.is_set():
        time.sleep(0.1)
