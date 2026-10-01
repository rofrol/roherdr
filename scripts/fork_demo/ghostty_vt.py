"""A minimal ctypes binding to libghostty-vt, the VT parser herdr vendors.

proxy.py feeds the herdr client's output into it so record.py can find what
to click by the text on screen; each cell also has its style. Only the calls the demo needs are bound; see
vendor/libghostty-vt/include/ghostty/vt/ for the C API.
"""

import ctypes
from ctypes import POINTER, Structure, Union, byref, c_bool, c_int, c_size_t, c_uint8, c_uint16
from ctypes import c_uint32, c_uint64, c_void_p
from dataclasses import dataclass

SUCCESS = 0
POINT_TAG_ACTIVE = 0
STYLE_COLOR_PALETTE = 1
STYLE_COLOR_RGB = 2
CELL_DATA_WIDE = 3
CELL_WIDE_SPACER_TAIL = 2


class Rgb(Structure):
    _fields_ = [("r", c_uint8), ("g", c_uint8), ("b", c_uint8)]


class StyleColorValue(Union):
    _fields_ = [("palette", c_uint8), ("rgb", Rgb), ("_padding", c_uint64)]


class StyleColor(Structure):
    _fields_ = [("tag", c_int), ("value", StyleColorValue)]


class Style(Structure):
    _fields_ = [
        ("size", c_size_t),
        ("fg_color", StyleColor),
        ("bg_color", StyleColor),
        ("underline_color", StyleColor),
        ("bold", c_bool),
        ("italic", c_bool),
        ("faint", c_bool),
        ("blink", c_bool),
        ("inverse", c_bool),
        ("invisible", c_bool),
        ("strikethrough", c_bool),
        ("overline", c_bool),
        ("underline", c_int),
    ]


class PointCoordinate(Structure):
    _fields_ = [("x", c_uint16), ("y", c_uint32)]


class PointValue(Union):
    _fields_ = [("coordinate", PointCoordinate), ("_padding", c_uint64 * 2)]


class Point(Structure):
    _fields_ = [("tag", c_int), ("value", PointValue)]


class GridRef(Structure):
    _fields_ = [("size", c_size_t), ("node", c_void_p), ("x", c_uint16), ("y", c_uint16)]


@dataclass(frozen=True)
class Cell:
    """One cell: `data` is its grapheme ("" for the right half of a wide
    glyph), colors are (r, g, b), a palette index, or None for the default."""

    data: str
    fg: object
    bg: object
    bold: bool
    faint: bool
    inverse: bool


def _color(value):
    if value.tag == STYLE_COLOR_RGB:
        rgb = value.value.rgb
        return (rgb.r, rgb.g, rgb.b)
    if value.tag == STYLE_COLOR_PALETTE:
        return value.value.palette
    return None


class Screen:
    """A terminal of a fixed size that parses bytes with libghostty-vt."""

    def __init__(self, library_path, cols, rows):
        self.lib = lib = ctypes.CDLL(library_path)
        lib.ghostty_terminal_new.argtypes = [c_void_p, POINTER(c_void_p), c_uint16, c_uint16]
        lib.ghostty_terminal_vt_write.argtypes = [c_void_p, ctypes.c_char_p, c_size_t]
        lib.ghostty_terminal_grid_ref.argtypes = [c_void_p, Point, POINTER(GridRef)]
        lib.ghostty_grid_ref_cell.argtypes = [POINTER(GridRef), POINTER(c_uint64)]
        lib.ghostty_grid_ref_style.argtypes = [POINTER(GridRef), POINTER(Style)]
        lib.ghostty_grid_ref_graphemes.argtypes = [
            POINTER(GridRef), POINTER(c_uint32), c_size_t, POINTER(c_size_t)
        ]
        lib.ghostty_cell_get.argtypes = [c_uint64, c_int, c_void_p]
        lib.ghostty_terminal_resize.argtypes = [c_void_p, c_uint16, c_uint16, c_uint32, c_uint32]
        self.cols, self.rows = cols, rows
        self.terminal = c_void_p()
        if lib.ghostty_terminal_new(None, byref(self.terminal), cols, rows) != SUCCESS:
            raise RuntimeError("ghostty_terminal_new failed")
        self._cells = None

    def resize(self, cols, rows, cell_width_px=0, cell_height_px=0):
        self.lib.ghostty_terminal_resize(self.terminal, cols, rows, cell_width_px, cell_height_px)
        self.cols, self.rows = cols, rows
        self._cells = None

    def feed(self, data):
        self.lib.ghostty_terminal_vt_write(self.terminal, data, len(data))
        self._cells = None

    def _read_cell(self, x, y):
        lib = self.lib
        point = Point(POINT_TAG_ACTIVE, PointValue(coordinate=PointCoordinate(x, y)))
        ref = GridRef(size=ctypes.sizeof(GridRef))
        if lib.ghostty_terminal_grid_ref(self.terminal, point, byref(ref)) != SUCCESS:
            return Cell(" ", None, None, False, False, False)
        raw = c_uint64()
        lib.ghostty_grid_ref_cell(byref(ref), byref(raw))
        wide = c_int()
        lib.ghostty_cell_get(raw, CELL_DATA_WIDE, byref(wide))
        style = Style(size=ctypes.sizeof(Style))
        lib.ghostty_grid_ref_style(byref(ref), byref(style))
        text = ""
        if wide.value != CELL_WIDE_SPACER_TAIL:
            buf = (c_uint32 * 16)()
            length = c_size_t()
            if lib.ghostty_grid_ref_graphemes(byref(ref), buf, 16, byref(length)) == SUCCESS:
                text = "".join(chr(cp) for cp in buf[: length.value])
            text = text or " "
        return Cell(text, _color(style.fg_color), _color(style.bg_color), style.bold,
                    style.faint, style.inverse)

    @property
    def cells(self):
        """Rows of Cell, read once per change of the screen."""
        if self._cells is None:
            self._cells = [[self._read_cell(x, y) for x in range(self.cols)] for y in range(self.rows)]
        return self._cells

    @property
    def display(self):
        """Screen text per row; a wide glyph is one character, as in pyte."""
        return ["".join(cell.data for cell in row) for row in self.cells]
