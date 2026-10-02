# Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
# Copyright (C) 2026 Roch Studio
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program.  If not, see <https://www.gnu.org/licenses/>.

"""Shared Roch NVRAM colors and Qt widget styling."""

import ctypes
import sys

from PySide6.QtCore import QSettings
from PySide6.QtGui import QColor, QFont, QPalette
from PySide6.QtWidgets import QApplication, QWidget


def load_dark_mode(settings: QSettings | None = None) -> bool:
    """Fresh installs start light; a stored choice remains authoritative."""
    return (settings or QSettings()).value("darkMode", False, type=bool)


def is_dark() -> bool:
    app = QApplication.instance()
    return bool(app and app.property("dark_mode"))


def semantic_color(tone: str) -> QColor:
    colors = {
        "muted": ("#62666d", "#a0a0a0"),
        "warning": ("#a85a14", "#f4aa65"),
        "queued": ("#b91c1c", "#ff5a5f"),
    }
    light, dark = colors[tone]
    return QColor(dark if is_dark() else light)


def sync_title_bar(window: QWidget) -> None:
    """Give the native Windows caption the same colors as the active theme."""
    if sys.platform != "win32":
        return
    dark = is_dark()
    handle = int(window.winId())
    try:
        dwm = ctypes.windll.dwmapi.DwmSetWindowAttribute
        dwm.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_void_p, ctypes.c_uint]
        dwm.restype = ctypes.c_long
        native_handle = ctypes.c_void_p(handle)
        enabled = ctypes.c_int(int(dark))
        # 20 is supported by current Windows; older Windows 10 builds use 19.
        if dwm(native_handle, 20, ctypes.byref(enabled), ctypes.sizeof(enabled)) != 0:
            dwm(native_handle, 19, ctypes.byref(enabled), ctypes.sizeof(enabled))

        def colorref(hex_color: str) -> ctypes.c_uint:
            color = QColor(hex_color)
            return ctypes.c_uint(color.red() | (color.green() << 8) | (color.blue() << 16))

        caption = colorref("#000000" if dark else "#ffffff")
        foreground = colorref("#ffffff" if dark else "#000000")
        # Caption and text colors are supported on Windows 11. The immersive
        # mode attribute above still supplies a dark title bar on Windows 10.
        dwm(native_handle, 35, ctypes.byref(caption), ctypes.sizeof(caption))
        dwm(native_handle, 36, ctypes.byref(foreground), ctypes.sizeof(foreground))
    except (AttributeError, OSError):
        # A Qt window must still open on systems without these DWM attributes.
        pass


def apply_theme(app: QApplication, dark: bool) -> None:
    """Style both Qt widgets and palette-backed native-looking controls."""
    app.setProperty("dark_mode", dark)
    app.setStyle("Fusion")
    font = QFont("Consolas")
    font.setPixelSize(12)
    app.setFont(font)
    colors = (
        {
            "window": "#000000", "surface": "#000000", "field": "#000000",
            "alternate": "#000000", "border": "#383838", "text": "#ffffff",
            "muted": "#ffffff", "disabled": "#a0a0a0", "hover": "#222222",
            "selected": "#d0343a", "red": "#ff5a5f", "red_hover": "#e0383e",
            "warning_bg": "#382719", "warning": "#f4aa65",
        }
        if dark else
        {
            "window": "#ffffff", "surface": "#ffffff", "field": "#ffffff",
            "alternate": "#ffffff", "border": "#d0d0d0", "text": "#000000",
            "muted": "#000000", "disabled": "#62666d", "hover": "#e6e6e6",
            "selected": "#d0343a", "red": "#b91c1c", "red_hover": "#e0383e",
            "warning_bg": "#fff1df", "warning": "#9a4b0c",
        }
    )

    palette = QPalette()
    for role, key in (
        (QPalette.Window, "window"), (QPalette.WindowText, "text"),
        (QPalette.Base, "field"), (QPalette.AlternateBase, "alternate"),
        (QPalette.ToolTipBase, "surface"), (QPalette.ToolTipText, "text"),
        (QPalette.Text, "text"), (QPalette.Button, "surface"),
        (QPalette.ButtonText, "text"), (QPalette.Highlight, "selected"),
    ):
        palette.setColor(role, QColor(colors[key]))
    palette.setColor(QPalette.HighlightedText, QColor("#ffffff"))
    palette.setColor(QPalette.PlaceholderText, QColor(colors["muted"]))
    palette.setColor(QPalette.Disabled, QPalette.Text, QColor(colors["disabled"]))
    palette.setColor(QPalette.Disabled, QPalette.ButtonText, QColor(colors["disabled"]))
    app.setPalette(palette)

    app.setStyleSheet("""
        QWidget { background: %(window)s; color: %(text)s;
                  font-family: Consolas; font-size: 12px; }
        QMainWindow, QDialog { background: %(window)s; }
        QLabel { background: transparent; }
        QLabel#brand { color: %(text)s; font-size: 12px; font-weight: 700; }
        QWidget#rochTitleBar { background: %(window)s; }
        QLabel#profileLine { color: %(muted)s; font-weight: 600; }
        QLabel#profileLine[tone="warning"] { color: %(warning)s; }
        QLabel#mutedText { color: %(muted)s; }
        QLabel#warningText { color: %(warning)s; font-weight: 600; }
        QLabel#footerDivider { color: %(muted)s; }
        QLabel#modeBanner { background: %(warning_bg)s; color: %(warning)s;
                            border: 1px solid %(warning)s; padding: 9px; font-weight: 700; }
        QLabel[tone="warning"] { color: %(warning)s; }
        QLabel[tone="queued"] { color: %(red)s; }
        QLabel[tone="muted"] { color: %(disabled)s; }
        QMenuBar { background: %(window)s; border-bottom: 1px solid %(border)s; padding: 3px; }
        QMenuBar::item { padding: 5px 11px; }
        QMenuBar::item:selected, QMenu::item:selected { background: %(hover)s; }
        QMenu { background: %(surface)s; border: 1px solid %(border)s; padding: 4px; }
        QMenu::item { padding: 5px 18px; }
        QStatusBar { background: %(surface)s; border-top: 1px solid %(border)s; color: %(muted)s; }
        QStatusBar QWidget { background: transparent; }
        QTabWidget::pane { border: 1px solid %(border)s; background: %(surface)s; }
        QTabBar::tab { background: %(surface)s; color: %(text)s;
                       border: 1px solid %(border)s; border-radius: 5px;
                       font-size: 13px; font-weight: 700;
                       padding: 5px 8px; margin-right: 3px; }
        QTabBar::tab:hover { color: %(text)s; background: %(hover)s; }
        QTabBar::tab:selected { color: #ffffff; background: %(selected)s;
                               border-color: %(selected)s; }
        QLineEdit, QComboBox { background: %(field)s; color: %(text)s;
                               border: 1px solid %(border)s; border-radius: 2px;
                               min-height: 26px; padding: 2px 8px;
                               selection-background-color: %(selected)s; selection-color: #ffffff; }
        QLineEdit:focus, QComboBox:focus { border: 1px solid %(red)s; }
        QLineEdit:disabled, QComboBox:disabled { color: %(disabled)s; background: %(surface)s; }
        QComboBox::drop-down { border-left: 1px solid %(border)s; width: 23px; }
        QComboBox QAbstractItemView { background: %(surface)s; color: %(text)s;
                                      border: 1px solid %(border)s;
                                      selection-background-color: %(selected)s; selection-color: #ffffff; }
        QPushButton { background: %(surface)s; color: %(text)s;
                      border: 1px solid %(border)s; border-radius: 3px;
                      min-height: 27px; padding: 3px 12px; font-weight: 600; }
        QPushButton:hover { background: %(hover)s; border-color: %(muted)s; }
        QPushButton:pressed { background: %(selected)s; }
        QPushButton:disabled { background: %(surface)s; color: %(disabled)s;
                               border-color: %(border)s; }
        QPushButton[accent="true"] { background: %(selected)s; color: #ffffff;
                                      border-color: %(selected)s; }
        QPushButton[accent="true"]:hover { background: %(red_hover)s; border-color: %(red_hover)s; }
        QPushButton[accent="true"]:disabled { background: %(surface)s;
                                              color: %(disabled)s; border-color: %(border)s; }
        QPushButton#themeToggle, QPushButton#captionButton, QPushButton#closeCaption {
            background: transparent; color: %(text)s; border: 0;
            border-radius: 0; padding: 0; min-height: 0;
            font-family: "Segoe UI Symbol"; font-size: 18px; font-weight: 400;
        }
        QPushButton#closeCaption { font-size: 20px; }
        QPushButton#themeToggle:hover, QPushButton#captionButton:hover { background: %(hover)s; }
        QPushButton#closeCaption:hover { background: #c42b1c; color: #ffffff; }
        QPushButton[social="true"] { background: transparent; color: %(selected)s;
                                     border: 0; min-height: 0; padding: 0 2px;
                                     font-weight: 700; }
        QPushButton[social="true"]:hover { color: %(red_hover)s; background: transparent; }
        QTableView { background: %(field)s; alternate-background-color: %(alternate)s;
                     color: %(text)s; border: 1px solid %(border)s; gridline-color: %(border)s;
                     selection-background-color: %(selected)s; selection-color: #ffffff; }
        QTableView::item { padding: 3px 5px; }
        QTableView::item:hover { background: %(hover)s; }
        QHeaderView::section { background: %(surface)s; color: %(text)s;
                               border: 0; border-right: 1px solid %(border)s;
                               border-bottom: 1px solid %(border)s; padding: 6px; font-weight: 700; }
        QTableCornerButton::section { background: %(surface)s; border: 1px solid %(border)s; }
        QGroupBox { background: %(surface)s; border: 1px solid %(border)s;
                    margin-top: 15px; padding: 15px 9px 9px 9px; font-weight: 700; }
        QGroupBox::title { subcontrol-origin: margin; left: 12px; padding: 0 5px;
                           color: %(text)s; }
        QScrollArea, QScrollArea > QWidget > QWidget { background: %(window)s; border: 0; }
        QScrollBar:vertical { background: %(window)s; width: 11px; }
        QScrollBar::handle:vertical { background: %(border)s; min-height: 24px; border-radius: 3px; }
        QScrollBar:horizontal { background: %(window)s; height: 11px; }
        QScrollBar::handle:horizontal { background: %(border)s; min-width: 24px; border-radius: 3px; }
        QScrollBar::add-line, QScrollBar::sub-line { width: 0; height: 0; }
        QToolTip { background: %(surface)s; color: %(text)s; border: 1px solid %(border)s; }
    """ % colors)
