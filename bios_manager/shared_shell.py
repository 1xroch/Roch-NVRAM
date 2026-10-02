"""Viewer-sized title and status strips for the Roch desktop tools."""

from PySide6.QtCore import Qt, QTimer, QUrl, QSize
from PySide6.QtGui import QDesktopServices, QFont, QIcon, QPainter, QPainterPath, QPixmap
from PySide6.QtWidgets import (
    QHBoxLayout, QLabel, QPushButton, QSizePolicy, QStatusBar, QWidget,
)


def _solid_moon_icon(device_ratio=1.0):
    """Render the shared 18px filled crescent without a font or emoji glyph."""
    outside = QPainterPath()
    outside.addEllipse(3.875, 1.0, 16.0, 16.0)
    cutout = QPainterPath()
    cutout.addEllipse(8.375, 1.0, 16.0, 16.0)
    crescent = outside.subtracted(cutout)
    icon = QIcon()
    # Keep the cutout transparent over normal and hovered caption backgrounds.
    for ratio in sorted({1.0, 2.0, 3.0, float(device_ratio)}):
        pixels = max(18, round(18 * ratio))
        pixmap = QPixmap(pixels, pixels)
        pixmap.fill(Qt.transparent)
        painter = QPainter(pixmap)
        painter.setRenderHint(QPainter.Antialiasing)
        painter.scale(pixels / 18.0, pixels / 18.0)
        painter.fillPath(crescent, Qt.black)
        painter.end()
        pixmap.setDevicePixelRatio(pixels / 18.0)
        icon.addPixmap(pixmap)
    return icon


class RochTitleBar(QWidget):
    def __init__(self, window, title, icon, toggle_theme):
        super().__init__(window)
        self.setObjectName("rochTitleBar")
        self.setFixedHeight(30)
        self.window = window
        row = QHBoxLayout(self)
        row.setContentsMargins(10, 0, 0, 0)
        row.setSpacing(0)
        self.mark = QLabel()
        self.mark.setFixedSize(24, 24)
        if icon is not None:
            self.mark.setPixmap(icon.pixmap(24, 24))
        row.addWidget(self.mark)
        self.title = QLabel(title)
        self.title.setObjectName("brand")
        self.title.setContentsMargins(10, 0, 0, 0)
        row.addWidget(self.title)
        row.addStretch(1)
        self.theme_button = QPushButton()
        self.theme_button.setObjectName("themeToggle")
        self.theme_button.setFixedSize(36, 30)
        self.theme_button.setIconSize(QSize(18, 18))
        self.moon_icon = _solid_moon_icon(self.devicePixelRatioF())
        symbol_font = QFont("Segoe UI Symbol")
        symbol_font.setPixelSize(18)
        self.theme_button.setFont(symbol_font)
        self.theme_button.clicked.connect(toggle_theme)
        row.addWidget(self.theme_button)
        for text, name, action in (
            ("\u2212", "Minimize", window.showMinimized),
            ("\u00d7", "Close", window.close),
        ):
            button = QPushButton(text)
            button.setObjectName("closeCaption" if name == "Close" else "captionButton")
            button.setFixedSize(44, 30)
            caption_font = QFont("Segoe UI Symbol")
            caption_font.setPixelSize(20 if name == "Close" else 18)
            button.setFont(caption_font)
            button.setToolTip(name)
            button.setAccessibleName(name)
            button.clicked.connect(action)
            if name == "Close":
                self.close_button = button
            else:
                self.minimize_button = button
            row.addWidget(button)

    def set_theme_target(self, dark):
        self.theme_button.setIcon(QIcon() if dark else self.moon_icon)
        self.theme_button.setText("\u2600" if dark else "")

    def mousePressEvent(self, event):
        if event.button() == Qt.LeftButton and self.window.windowHandle():
            self.window.windowHandle().startSystemMove()
            event.accept()
        else:
            super().mousePressEvent(event)

    def mouseDoubleClickEvent(self, event):
        if event.button() == Qt.LeftButton:
            if self.window.isMaximized():
                self.window.showNormal()
            else:
                self.window.showMaximized()
            event.accept()
        else:
            super().mouseDoubleClickEvent(event)


class RochFooter(QStatusBar):
    """Keep the status API and social links together in the Viewer-sized strip."""

    def __init__(self, window, links):
        super().__init__(window)
        self.setObjectName("rochFooter")
        self.setFixedHeight(24)
        self._message = ""
        social = QWidget()
        row = QHBoxLayout(social)
        row.setContentsMargins(10, 0, 0, 0)
        row.setSpacing(5)
        for index, (name, address) in enumerate(links):
            if index:
                divider = QLabel("|")
                divider.setObjectName("footerDivider")
                row.addWidget(divider)
            button = QPushButton(name)
            button.setProperty("social", True)
            button.setCursor(Qt.PointingHandCursor)
            button.setToolTip(address)
            button.clicked.connect(
                lambda _=False, url=address: QDesktopServices.openUrl(QUrl(url))
            )
            row.addWidget(button)
        self.addPermanentWidget(social)
        self.status_label = QLabel()
        self.status_label.setObjectName("mutedText")
        self.status_label.setAlignment(Qt.AlignRight | Qt.AlignVCenter)
        self.status_label.setSizePolicy(QSizePolicy.Ignored, QSizePolicy.Preferred)
        self.addPermanentWidget(self.status_label, 1)

    def showMessage(self, message, timeout=0):
        self._message = message
        self.status_label.setText(message)
        self.status_label.setToolTip(message)
        self.messageChanged.emit(message)
        if timeout:
            QTimer.singleShot(timeout, lambda: self.clearMessage() if self._message == message else None)

    def clearMessage(self):
        self.showMessage("")

    def currentMessage(self):
        return self._message
