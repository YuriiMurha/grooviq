"""Reject release tags unless every app package records the same stable version."""
import json
import os
from pathlib import Path
import re
import sys
import tomllib


def validate(tag, versions):
    # Stable numeric versions keep Windows installer version semantics unambiguous.
    if not re.fullmatch(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", tag, flags=re.ASCII):
        raise ValueError("expected a stable version tag: vMAJOR.MINOR.PATCH")
    version = tag[1:]
    if any(int(part) > 65535 for part in version.split(".")):
        raise ValueError("version component exceeds Windows installer limit")
    if not versions or any(value != version for value in versions.values()):
        raise ValueError("tag must match every app version: " + repr(versions))
    return version


def read_versions(root):
    def json_file(path):
        return json.loads((root / path).read_text())

    lock = json_file("app/ui/package-lock.json")
    cargo_lock = tomllib.loads((root / "Cargo.lock").read_text())
    return {
        "tauri": json_file("app/src-tauri/tauri.conf.json")["version"],
        "cargo": tomllib.loads((root / "app/src-tauri/Cargo.toml").read_text())["package"]["version"],
        "cargo lock": next(p["version"] for p in cargo_lock["package"] if p["name"] == "grooviq-app"),
        "ui": json_file("app/ui/package.json")["version"],
        "npm lock": lock["version"],
        "npm lock root": lock["packages"][""]["version"],
    }


if __name__ == "__main__":
    try:
        if os.environ.get("GITHUB_REF_TYPE") != "tag":
            raise ValueError("release requires a tag ref")
        validate(os.environ["RELEASE_TAG"], read_versions(Path(__file__).resolve().parents[1]))
    except (ValueError, KeyError, StopIteration) as error:
        sys.exit(str(error))
