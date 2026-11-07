#!/usr/bin/env python3
"""Release validation, build reuse, asset collection and checksums (Python 3.11+)."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import sys
import tomllib
from urllib.parse import urlencode
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[1]
PLATFORMS = (("windows", "x86_64"), ("linux", "x86_64"),
             ("macos", "arm64"), ("macos", "x86_64"))


def find_linux_build(repository: str, sha: str, token: str,
                     api_url: str = "https://api.github.com") -> str:
    """Reuse only a successful main push build of this exact commit; otherwise build."""
    def request(path: str) -> dict:
        req = Request(f"{api_url.rstrip('/')}/repos/{repository}/{path}", headers={
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.github+json",
        })
        with urlopen(req, timeout=10) as response:
            return json.load(response)

    try:
        query = urlencode({"branch": "main", "event": "push", "status": "success",
                           "head_sha": sha, "per_page": 5})
        runs = request(f"actions/workflows/ci.yml/runs?{query}")["workflow_runs"]
        for run in runs:
            if (run.get("head_sha") != sha or run.get("head_branch") != "main"
                    or run.get("event") != "push" or run.get("conclusion") != "success"
                    or run.get("status") != "completed"):
                continue
            run_id = run["id"]
            if type(run_id) is not int or run_id <= 0:
                continue
            artifacts = request(f"actions/runs/{run_id}/artifacts?per_page=100")["artifacts"]
            if any(a.get("name") == "MakePort-linux-x86_64" and a.get("expired") is False
                   for a in artifacts):
                return str(run_id)
    except (OSError, ValueError, KeyError, TypeError, AttributeError) as error:
        # API or retention failures must not prevent a normal fresh build.
        print(f"Linux build lookup unavailable ({type(error).__name__}); building normally.",
              file=sys.stderr)
    return ""


def version_from_tag(tag: str) -> str:
    if not re.fullmatch(r"v(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", tag):
        raise ValueError("Tag must match vX.Y.Z, for example v0.1.0.")
    return tag[1:]


def validate(tag: str, root: Path = ROOT) -> None:
    version = version_from_tag(tag)
    manifest = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    package = manifest["package"]
    if package["version"] != version:
        raise ValueError(f"Tag {tag} does not match Cargo.toml version {package['version']}.")
    lock = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))
    entries = [p for p in lock["package"] if p["name"] == package["name"] and "source" not in p]
    if len(entries) != 1 or entries[0]["version"] != version:
        raise ValueError("Cargo.lock must contain the same project version as Cargo.toml.")


def asset_name(tag: str, system: str, architecture: str) -> str:
    version_from_tag(tag)
    if (system, architecture) not in PLATFORMS:
        raise ValueError("Unsupported release platform or architecture.")
    extension = ".exe" if system == "windows" else ""
    return f"MakePort-{tag}-{system}-{architecture}{extension}"


def collect(tag: str, system: str, architecture: str, root: Path = ROOT) -> Path:
    name = asset_name(tag, system, architecture)
    source = root / "target" / "release" / ("makeport.exe" if system == "windows" else "makeport")
    if not source.is_file():
        raise FileNotFoundError(f"Missing executable: {source}")
    directory = root / "release-assets"
    directory.mkdir(exist_ok=True)
    target = directory / name
    if target.exists():
        raise FileExistsError(f"Release asset already exists: {target}")
    shutil.copy2(source, target)
    return target


def checksums(tag: str, root: Path = ROOT) -> Path:
    directory = root / "release-assets"
    expected = {asset_name(tag, system, arch) for system, arch in PLATFORMS}
    actual = {p.name for p in directory.iterdir() if p.name != "SHA256SUMS"}
    if actual != expected or any(not (directory / name).is_file() for name in expected):
        raise ValueError(f"Release assets differ from the expected set. Missing: {sorted(expected-actual)}; unexpected: {sorted(actual-expected)}")
    lines = []
    for name in sorted(expected):
        with (directory / name).open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        lines.append(f"{digest}  {name}\n")
    output = directory / "SHA256SUMS"
    output.write_text("".join(lines), encoding="utf-8", newline="\n")
    return output


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("validate").add_argument("tag")
    commands.add_parser("find-linux-build")
    collect_parser = commands.add_parser("collect")
    collect_parser.add_argument("tag")
    collect_parser.add_argument("system")
    collect_parser.add_argument("architecture")
    commands.add_parser("checksums").add_argument("tag")
    args = parser.parse_args()
    try:
        if args.command == "validate":
            validate(args.tag)
            print(f"Validated release {args.tag}")
        elif args.command == "find-linux-build":
            print(find_linux_build(os.environ["GITHUB_REPOSITORY"], os.environ["GITHUB_SHA"],
                                   os.environ["GITHUB_TOKEN"],
                                   os.environ.get("GITHUB_API_URL", "https://api.github.com")))
        elif args.command == "collect":
            print(collect(args.tag, args.system, args.architecture))
        else:
            print(checksums(args.tag))
    except (OSError, ValueError, KeyError) as error:
        parser.exit(1, f"Release error: {error}\n")


if __name__ == "__main__":
    main()
