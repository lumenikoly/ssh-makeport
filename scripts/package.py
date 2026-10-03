#!/usr/bin/env python3
"""Package an already-built executable; requires only Python's standard library."""
from __future__ import annotations
import argparse
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import zipfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def package(binary: Path, system: str, architecture: str, output: Path) -> Path:
    if not binary.is_file():
        raise FileNotFoundError(f"Нет {binary}. Сначала выполните cargo build --release.")
    name = f"MakePort-{system}-{architecture}"
    stage = output / name
    if stage.exists():
        shutil.rmtree(stage)
    stage.mkdir(parents=True)
    for filename in ("LICENSE", "README.md", "THIRD_PARTY.md"):
        shutil.copy2(ROOT / filename, stage / filename)
    if (ROOT / "Cargo.lock").is_file():
        shutil.copy2(ROOT / "Cargo.lock", stage / "Cargo.lock")
    if system == "macos":
        app = stage / "MakePort.app"
        contents = app / "Contents"
        executable = contents / "MacOS" / "makeport"
        executable.parent.mkdir(parents=True)
        shutil.copy2(binary, executable)
        executable.chmod(0o755)
        (contents / "Resources").mkdir()
        version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
        info = {
            "CFBundleExecutable": "makeport",
            "CFBundleIdentifier": "app.makeport.desktop",
            "CFBundleName": "MakePort",
            "CFBundleDisplayName": "MakePort",
            "CFBundlePackageType": "APPL",
            "CFBundleShortVersionString": version,
            "CFBundleVersion": version,
            "LSMinimumSystemVersion": "12.0",
            "NSHighResolutionCapable": True,
        }
        with (contents / "Info.plist").open("wb") as stream:
            plistlib.dump(info, stream)
        if platform.system() == "Darwin":
            subprocess.run(["/usr/bin/codesign", "--force", "--sign", "-", str(app)], check=True)
    else:
        executable = stage / ("makeport.exe" if system == "windows" else "makeport")
        shutil.copy2(binary, executable)
        if system != "windows":
            executable.chmod(0o755)
            shutil.copy2(ROOT / "packaging" / "makeport.desktop", stage / "makeport.desktop")
    archive = output / f"{name}.zip"
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as target:
        for file in sorted(stage.rglob("*")):
            if file.is_file():
                target.write(file, file.relative_to(output))
    return archive


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--os", choices=("linux", "macos", "windows"))
    parser.add_argument("--arch", default=platform.machine().lower())
    parser.add_argument("--output", type=Path, default=ROOT / "dist")
    args = parser.parse_args()
    system = args.os or {"Linux": "linux", "Darwin": "macos", "Windows": "windows"}.get(platform.system())
    if system is None:
        parser.error("Укажите поддерживаемую ОС через --os")
    binary = args.binary or ROOT / "target" / "release" / ("makeport.exe" if system == "windows" else "makeport")
    try:
        print(package(binary, system, args.arch, args.output))
    except (OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Ошибка упаковки: {error}\n")


if __name__ == "__main__":
    main()
