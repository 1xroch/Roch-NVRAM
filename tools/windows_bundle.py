# Copyright (C) 2026 Roch Studio
# SPDX-License-Identifier: GPL-3.0-or-later
"""Keep Windows ICU dependencies from being shadowed by the build machine's ICU.

Qt's Windows wheels import unversioned functions from the Windows ICU libraries.
An unrelated icuuc.dll on PATH can instead export names such as ucnv_open_78.
PyInstaller can collect that DLL despite the incompatible ABI. Windows 11 may
mask the mistake by resolving ICU differently, so check the package statically.
"""
from __future__ import annotations

import argparse
import os
from pathlib import Path, PureWindowsPath
import re

import pefile

WINDOWS_ICU = {"icu.dll", "icuuc.dll", "icuin.dll"}


def is_icu_payload(name: str) -> bool:
    leaf = PureWindowsPath(name).name.lower()
    return leaf in WINDOWS_ICU or re.fullmatch(r"icudt\d+\.dll", leaf) is not None


def validate_system_icu_imports(name: str, data: bytes, cache: dict) -> None:
    """Refuse removal if any retained binary needs a non-Windows ICU ABI."""
    if not name.lower().endswith((".dll", ".pyd")):
        return
    with pefile.PE(data=data, fast_load=True) as binary:
        binary.parse_data_directories(
            directories=[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_IMPORT"]]
        )
        for dependency in getattr(binary, "DIRECTORY_ENTRY_IMPORT", []):
            dll = dependency.dll.decode("ascii").lower()
            if not is_icu_payload(dll):
                continue
            if dll not in WINDOWS_ICU:
                raise RuntimeError(f"{name} needs non-system ICU {dll}; cannot remove it")
            if dll not in cache:
                system_dll = Path(os.environ["SystemRoot"]) / "System32" / dll
                with pefile.PE(str(system_dll), fast_load=True) as system:
                    system.parse_data_directories(
                        directories=[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_EXPORT"]]
                    )
                    cache[dll] = {
                        symbol.name if symbol.name else symbol.ordinal
                        for symbol in system.DIRECTORY_ENTRY_EXPORT.symbols
                    }
            missing = [
                symbol.name if symbol.name else symbol.ordinal
                for symbol in dependency.imports
                if (symbol.name if symbol.name else symbol.ordinal) not in cache[dll]
            ]
            if missing:
                raise RuntimeError(f"{name} needs exports absent from Windows {dll}: {missing}")


def use_windows_icu(binaries: list) -> list:
    """Filter Analysis.binaries only after checking the replacement's exports."""
    kept = [entry for entry in binaries if not is_icu_payload(entry[0])]
    cache = {}
    for destination, source, kind in kept:
        if kind == "BINARY":
            validate_system_icu_imports(destination, Path(source).read_bytes(), cache)
    removed = [entry[0] for entry in binaries if is_icu_payload(entry[0])]
    print(f"Windows ICU: using OS libraries; excluded bundled files: {removed}")
    return kept


def validate_archive(path: Path) -> None:
    from PyInstaller.archive.readers import CArchiveReader

    archive = CArchiveReader(str(path))
    unwanted = [name for name in archive.toc if is_icu_payload(name)]
    if unwanted:
        raise RuntimeError(f"Package shadows Windows ICU: {unwanted}")
    core = [name for name in archive.toc if PureWindowsPath(name).name.lower() == "qt6core.dll"]
    if len(core) != 1:
        raise RuntimeError(f"Expected one Qt6Core.dll, found {core}")
    cache = {}
    for name in archive.toc:
        if name.lower().endswith((".dll", ".pyd")):
            validate_system_icu_imports(name, archive.extract(name), cache)
    print(f"PASS: {path.name} uses Windows ICU without bundled shadow DLLs")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("executable", type=Path)
    args = parser.parse_args()
    validate_archive(args.executable)
