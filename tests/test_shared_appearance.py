"""Theme persistence and shell regressions without SCEWIN or user settings."""
import os
os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

try:
    from PySide6.QtCore import QSettings, Qt, QSize
    from PySide6.QtGui import QPalette
    from PySide6.QtWidgets import QApplication, QPushButton
    from bios_manager.gui import MainWindow
    from bios_manager.theme import apply_theme, load_dark_mode
    GUI_AVAILABLE = True
except ImportError:
    GUI_AVAILABLE = False


@unittest.skipUnless(GUI_AVAILABLE, "PySide6 is not installed")
class SharedAppearanceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = QApplication.instance() or QApplication([])

    def test_fresh_light_and_saved_dark_preserve_unrelated_settings(self):
        with tempfile.TemporaryDirectory() as folder:
            settings = QSettings(str(Path(folder) / "appearance.ini"), QSettings.IniFormat)
            settings.setValue("unrelated", "keep")
            self.assertFalse(load_dark_mode(settings))
            settings.setValue("darkMode", True)
            settings.sync()
            reopened = QSettings(settings.fileName(), QSettings.IniFormat)
            self.assertTrue(load_dark_mode(reopened))
            self.assertEqual(reopened.value("unrelated"), "keep")

    def test_both_palettes_use_viewer_font_and_shared_colors(self):
        for dark, background, text, disabled, border in (
            (False, "#ffffff", "#000000", "#62666d", "#d0d0d0"),
            (True, "#000000", "#ffffff", "#a0a0a0", "#383838"),
        ):
            apply_theme(self.app, dark)
            self.assertEqual(self.app.font().family(), "Consolas")
            self.assertEqual(self.app.font().pixelSize(), 12)
            for role in (QPalette.Window, QPalette.Base, QPalette.AlternateBase, QPalette.Button):
                self.assertEqual(self.app.palette().color(role).name(), background)
            for role in (QPalette.WindowText, QPalette.Text, QPalette.ButtonText):
                self.assertEqual(self.app.palette().color(role).name(), text)
            self.assertEqual(self.app.palette().color(QPalette.Disabled, QPalette.Text).name(), disabled)
            self.assertEqual(self.app.palette().color(QPalette.Highlight).name(), "#d0343a")
            self.assertIn(border, self.app.styleSheet())

    def test_same_shell_retains_menus_and_disabled_initial_controls(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            apply_theme(self.app, False)
            backend = SimpleNamespace(app_root=root, backup_root=root / "backups")
            with patch("bios_manager.gui.sync_title_bar"):
                window = MainWindow(None, None, backend=backend)
                window.show()
                self.app.processEvents()
                self.assertEqual(window.title_bar.height(), 30)
                self.assertEqual(window.statusBar().height(), 24)
                self.assertEqual(window.title_bar.mark.width(), 24)
                self.assertEqual(window.theme_button.width(), 36)
                for button, width, size, glyph in (
                    (window.theme_button, 36, 18, ""),
                    (window.title_bar.minimize_button, 44, 18, "\u2212"),
                    (window.title_bar.close_button, 44, 20, "\u00d7"),
                ):
                    self.assertEqual((button.width(), button.height()), (width, 30))
                    self.assertEqual(button.font().family(), "Segoe UI Symbol")
                    self.assertEqual(button.font().pixelSize(), size)
                    self.assertEqual(button.text(), glyph)
                moon = window.theme_button.icon().pixmap(QSize(18, 18)).toImage()
                self.assertEqual(window.theme_button.iconSize(), QSize(18, 18))
                self.assertEqual(moon.pixelColor(6, 9).getRgb(), (0, 0, 0, 255))
                self.assertEqual(moon.pixelColor(12, 9).alpha(), 0)
                self.assertEqual([action.text() for action in window.menu_bar.actions()], ["File", "View"])
                buttons = {button.text(): button for button in window.findChildren(QPushButton)}
                self.assertTrue(buttons["Export"].isEnabled())
                for label in ("Import", "Queue selected change", "Load NVRAM..."):
                    self.assertFalse(buttons[label].isEnabled(), label)
                self.assertIn("Queued changes: 0", window.statusBar().currentMessage())
                self.assertTrue(window.windowFlags() & Qt.FramelessWindowHint)
                settings = QSettings(str(root / "appearance.ini"), QSettings.IniFormat)
                settings.setValue("unrelated", "keep")
                with patch("bios_manager.gui.QSettings", return_value=settings):
                    window.theme_button.click()
                    self.app.processEvents()
                    self.assertTrue(window.dark_mode_action.isChecked())
                    self.assertEqual(window.theme_button.text(), "\u2600")
                    self.assertTrue(window.theme_button.icon().isNull())
                    self.assertEqual(window.theme_button.accessibleName(), "Switch to light mode")
                    self.assertTrue(settings.value("darkMode", False, type=bool))
                    self.assertEqual(settings.value("unrelated"), "keep")
                    window.theme_button.click()
                    self.assertFalse(settings.value("darkMode", True, type=bool))
                window.close()


if __name__ == "__main__":
    unittest.main()
