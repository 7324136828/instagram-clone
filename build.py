#!/usr/bin/env python3
"""Build portable Debug (local) and Release (LAN) Windows applications into build/dist."""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import venv
import uuid
from pathlib import Path

from backend.settings import SettingsError, load_port_mapping

ROOT = Path(__file__).resolve().parent
FRONTEND = ROOT / "frontend"
BACKEND = ROOT / "backend"
WORK = ROOT / "build"
OUTPUT = WORK / "dist"
# Output folder of builds made before build/dist; never modified, only mentioned.
LEGACY_OUTPUT = ROOT / "dist"
LOCAL_VENV = ROOT / ".venv"
LOCAL_PYTHON = LOCAL_VENV / "Scripts" / "python.exe"
APPLICATION = "server"
PORT_MAPPING = ROOT / "port_mapping.json"
PROFILES = {"Debug": "127.0.0.1", "Release": "0.0.0.0"}
ARTIFACTS = (
    "server.exe", "public", "run.bat", "run-frontend.bat", "run-backend.bat",
    "runtime.json", "port_mapping.json", "README.txt",
)

# A "%" in the folder path is refused: the one-file server.exe cannot start from such a path
# (each copy fails to find its unpacked files and starts another copy, without end). The path is
# tested with delayed expansion, which never expands it a second time ("call" or a pipe would).
# Role launchers start server.exe themselves rather than "call run.bat", for the same reason.
LAUNCHER_TEMPLATE = """@echo off
setlocal
{comment}set "HERE=%~dp0"
setlocal EnableDelayedExpansion
if not "!HERE:%%=!"=="!HERE!" (
  echo [run] ERROR: The path of this folder contains a %% sign, so server.exe cannot start from it. Rename or move the folder. 1>&2
  exit /b 1
)
endlocal
if not exist "%~dp0server.exe" (
  echo [run] ERROR: Backend executable is missing. Rebuild with build.bat. 1>&2
  exit /b 1
)
if /i "%~1"=="--help" if "%~2"=="" goto launch
if /i "%~1"=="-h" if "%~2"=="" goto launch
"%~dp0server.exe" --initialize-settings
if errorlevel 1 exit /b %ERRORLEVEL%
:launch
"%~dp0server.exe" {role}%*
exit /b %ERRORLEVEL%
"""

LAUNCHER = LAUNCHER_TEMPLATE.format(comment="", role="")


def role_launcher(role: str) -> str:
    return LAUNCHER_TEMPLATE.format(
        comment=f"rem Runs only the {role} of the application. Extra arguments are passed to server.exe.\n",
        role=f"--role {role} ",
    )


LAUNCHERS = {
    "run.bat": LAUNCHER,
    "run-frontend.bat": role_launcher("frontend"),
    "run-backend.bat": role_launcher("backend"),
}

PROFILE_NOTES = {
    "Debug": "Debug profile: listens on 127.0.0.1, so only this computer can connect.",
    "Release": (
        "Release profile: listens on 0.0.0.0, so other devices on your network can\n"
        "connect. Windows may ask you to allow server.exe through the firewall.\n"
        "There is no password: anyone on the same network who can reach these ports\n"
        "can view the imported posts, import more and clear the library. On networks\n"
        "you do not trust (hotel, office, cafe Wi-Fi) use the Debug build instead.\n"
        "Web pages from other sites can neither read nor change the library."
    ),
}

# Raw string: the Windows paths below contain backslashes.
README = r"""Instagram Clone - portable {profile} build
=====================================================================

This folder runs on 64-bit Windows 10/11 without Python or Node.js. Copy the
WHOLE folder: server.exe needs public\, runtime.json and port_mapping.json
beside it. The folder path must not contain a % sign.

{note}

Start
-----
  run.bat              frontend + backend in one process
  run-backend.bat      backend (API and library) only
  run-frontend.bat     frontend only; /api requests are forwarded to a backend
  run.bat --help       all options

Ports
-----
Default ports come from port_mapping.json in this folder (frontend {frontend_port},
backend {backend_port}). On the first start, missing ports are copied into
  %APPDATA%\instagram-clone\settings.json
and from then on settings.json wins; edit it to change the ports. Options win
over both, for example:
  run.bat --frontend 4000 --backend 9000
FRONTEND_PORT, BACKEND_PORT and BACKEND_HOST environment variables also work.

Frontend and backend on different machines
------------------------------------------
Use the Release build on both machines.
  Machine A:  run-backend.bat
              It prints its LAN address, e.g. http://192.168.1.20:{backend_port}
  Machine B:  run-frontend.bat --backend-url http://192.168.1.20:{backend_port}
              Then open the Frontend or LAN address it prints in a browser.
Instead of --backend-url, machine B can set
  "backend_url": "http://192.168.1.20:{backend_port}"
in its settings.json.

Imported posts (the library)
----------------------------
Imports are merged and saved in %TEMP%\instagram_cache on the machine that
runs the backend, which prints the folder as "Cache:" when it starts. Choose
another folder with INSTAGRAM_CACHE_DIR (an environment variable, or a line
INSTAGRAM_CACHE_DIR=D:\instagram_cache in a .env file in this folder) or with
"instagram_cache_directory" in settings.json. Give the library a folder of its
own: it refuses a folder that already holds other files (for example an
extracted export) and never deletes files it did not write. Only one running
copy of the app can use a library folder at a time.

Host names
----------
Open the app by IP address, localhost or this computer's name. To use another
host name (a DNS name or a reverse proxy), list it in settings.json:
  "allowed_hosts": ["photos.example.lan"]
Other names are refused, which protects the library from DNS rebinding.

Other data
----------
The click counter database is kept in "data_directory" from settings.json
(default %TEMP%\instagram-clone); COUNTER_DB_PATH in .env
overrides it. Files you add to this folder, such as .env, are kept when the
build is updated.
"""


def readme(profile: str, ports: dict[str, int]) -> str:
    return README.format(profile=profile, note=PROFILE_NOTES[profile], **ports)


class BuildError(RuntimeError):
    """Raised when a portable build cannot be completed."""


def validate_port_defaults(path: Path) -> dict[str, int]:
    try:
        return load_port_mapping(path)
    except SettingsError as error:
        raise BuildError(f"Invalid port defaults: {error}") from error


def checked_path(path: Path, parent: Path) -> Path:
    """Reject symlinks/junctions that resolve outside the intended directory."""
    resolved = path.resolve()
    allowed = parent.resolve()
    if resolved == allowed or not resolved.is_relative_to(allowed):
        raise BuildError(f"Unsafe build path outside {allowed}: {path}")
    return resolved


def run(
    command: list[str | Path], *, cwd: Path = ROOT,
    env: dict[str, str] | None = None,
) -> None:
    rendered = [str(part) for part in command]
    print(f"[build] $ {subprocess.list2cmdline(rendered)}", flush=True)
    completed = subprocess.run(rendered, cwd=cwd, env=env, check=False)
    if completed.returncode:
        raise BuildError(f"Command failed with exit code {completed.returncode}")


def select_python() -> Path:
    if (
        sys.prefix != getattr(sys, "base_prefix", sys.prefix)
        or os.environ.get("VIRTUAL_ENV")
        or os.environ.get("CONDA_PREFIX")
    ):
        print(f"[build] Using active Python environment: {sys.prefix}")
        return Path(sys.executable)
    if not LOCAL_PYTHON.is_file():
        checked_path(LOCAL_VENV, ROOT)
        print(f"[build] Creating Python environment at {LOCAL_VENV}")
        venv.EnvBuilder(with_pip=True).create(LOCAL_VENV)
    if not LOCAL_PYTHON.is_file():
        raise BuildError(f"Python environment has no interpreter: {LOCAL_PYTHON}")
    return LOCAL_PYTHON


def pyinstaller_command(python: Path, stage: Path) -> list[str | Path]:
    command: list[str | Path] = [
        python,
        "-m",
        "PyInstaller",
        "--noconfirm",
        "--clean",
        "--onefile",
        "--console",
        "--noupx",
        "--name", APPLICATION,
        "--distpath", stage / "frozen",
        "--workpath", stage / "pyinstaller",
        "--specpath", stage,
        "--paths", ROOT,
    ]
    # Uvicorn loads these configured implementations by their string names.
    for module in (
        "uvicorn.logging",
        "uvicorn.loops.asyncio",
        "uvicorn.lifespan.on",
        "uvicorn.protocols.http.h11_impl",
    ):
        command.extend(["--hidden-import", module])
    command.append(BACKEND / "packaged.py")
    return command


def prepare_artifacts(stage: Path, *, defaults_path: Path = PORT_MAPPING) -> Path:
    """Create two portable packages from the shared frontend and executable."""
    ports = validate_port_defaults(defaults_path)
    packages = stage / "packages"
    checked_path(packages, stage)
    packages.mkdir()
    for profile, host in PROFILES.items():
        package = packages / profile
        checked_path(package, packages)
        package.mkdir()
        shutil.copy2(stage / "server.exe", package / "server.exe")
        shutil.copytree(stage / "public", package / "public")
        shutil.copy2(defaults_path, package / "port_mapping.json")
        for name, launcher in LAUNCHERS.items():
            (package / name).write_text(launcher, encoding="ascii", newline="\r\n")
        (package / "runtime.json").write_text(
            json.dumps({"host": host}, indent=2) + "\n", encoding="ascii",
        )
        (package / "README.txt").write_text(
            readme(profile, ports), encoding="ascii", newline="\r\n",
        )
    return packages


def validate_artifacts(stage: Path, *, host: str) -> None:
    if not (stage / "server.exe").is_file():
        raise BuildError("PyInstaller did not produce the backend executable")
    if not (stage / "public" / "index.html").is_file():
        raise BuildError("Vite did not produce public/index.html")
    if json.loads((stage / "runtime.json").read_text(encoding="ascii")) != {"host": host}:
        raise BuildError(f"Unexpected runtime defaults in {stage}")
    validate_port_defaults(stage / "port_mapping.json")
    batch_files = sorted(path.relative_to(stage) for path in stage.rglob("*")
                         if path.is_file() and path.suffix.lower() == ".bat")
    if batch_files != sorted(Path(name) for name in LAUNCHERS):
        raise BuildError(f"Unexpected batch files in portable artifacts: {batch_files}")
    for name, launcher in LAUNCHERS.items():
        if (stage / name).read_bytes() != launcher.replace("\n", "\r\n").encode("ascii"):
            raise BuildError(f"Unexpected launcher content in {stage / name}")
    readme_path = stage / "README.txt"
    if not readme_path.is_file() or not readme_path.read_text(encoding="ascii").strip():
        raise BuildError(f"Missing package README at {readme_path}")


def staging_directory(work: Path, output: Path) -> Path:
    """A new folder for one build's work files: inside work and never overlapping the output.

    It is deleted after a successful build, so it must never hold published packages.
    """
    stage = work / ("portable-" + uuid.uuid4().hex)
    resolved = checked_path(stage, work)
    published = output.resolve()
    if resolved.is_relative_to(published) or published.is_relative_to(resolved):
        raise BuildError(f"Staging folder {stage} overlaps the output folder {output}")
    return stage


def ensure_replaceable(path: Path) -> None:
    """Windows lets a running server.exe be renamed but not opened for writing: probe every
    file before anything is moved, so a running copy never leaves a half-replaced package."""
    if not path.is_file():
        return
    try:
        descriptor = os.open(path, os.O_RDWR | getattr(os, "O_BINARY", 0))
    except OSError as error:
        raise BuildError(
            f"{path} is in use, probably by the running application. Close it and retry. ({error})"
        ) from error
    os.close(descriptor)


STAGE_NAME = re.compile(r"portable-[0-9a-f]{32}")


def remove_build_files(stage: Path, work: Path) -> bool:
    """Best effort: True when the folder is gone; a locked file only leaves it behind."""
    shutil.rmtree(checked_path(stage, work), ignore_errors=True)
    return not stage.exists()


def remove_stale_stages(work: Path) -> None:
    """Removes work folders that earlier builds kept (failed builds, locked files)."""
    for entry in sorted(work.glob("portable-*")) if work.is_dir() else []:
        if not STAGE_NAME.fullmatch(entry.name) or entry.is_symlink() or not entry.is_dir():
            continue
        try:
            removed = remove_build_files(entry, work)
        except BuildError:
            continue
        if not removed:
            print(f"[build] Warning: could not remove old build files in {entry}")


def publish_artifacts(stage: Path, *, repository: Path = ROOT) -> None:
    """Publish both profiles together, preserving their data and local settings.

    Only the names in ARTIFACTS are replaced (by rename, with rollback); anything else in
    build/dist/<profile>, such as a .env or data the user added, is never touched.
    """
    work = repository / "build"
    output = work / "dist"
    checked_path(output, work)
    if checked_path(stage, work).is_relative_to(output.resolve()):
        raise BuildError(f"Staging folder {stage} must not be inside the output folder {output}")
    output.mkdir(exist_ok=True)
    previous = stage / "previous"
    checked_path(previous, stage)
    previous.mkdir()
    installed: list[tuple[str, str]] = []
    backed_up: list[tuple[str, str]] = []
    # Check every mutation target before moving anything. In particular, a
    # junction used for one output must not leave another output half updated.
    for profile in PROFILES:
        checked_path(stage / profile, stage)
        checked_path(output / profile, output)
        checked_path(previous / profile, previous)
        for name in ARTIFACTS:
            checked_path(stage / profile / name, stage / profile)
            checked_path(output / profile / name, output / profile)
            checked_path(previous / profile / name, previous / profile)
    for profile in PROFILES:
        for name in ARTIFACTS:
            ensure_replaceable(output / profile / name)
    for profile in PROFILES:
        (output / profile).mkdir(exist_ok=True)
        (previous / profile).mkdir()
    try:
        for profile in PROFILES:
            for name in ARTIFACTS:
                source = stage / profile / name
                target = output / profile / name
                backup = previous / profile / name
                checked_path(source, stage / profile)
                checked_path(target, output / profile)
                checked_path(backup, previous / profile)
                if target.exists():
                    target.rename(backup)
                    backed_up.append((profile, name))
                source.rename(target)
                installed.append((profile, name))
    except OSError as error:
        # A locked artifact in either profile restores both previous packages.
        # User state in each distribution is never moved.
        for profile, name in reversed(installed):
            target = output / profile / name
            checked_path(target, output / profile)
            target.rename(stage / profile / name)
        for profile, name in reversed(backed_up):
            backup = previous / profile / name
            checked_path(backup, previous / profile)
            backup.rename(output / profile / name)
        raise BuildError(
            "Could not replace Debug/Release artifacts; close the running application "
            f"and retry. {error}"
        ) from error


def build_packages(stage: Path, *, python: Path, npm: str) -> Path:
    """Build the frontend and executable into stage; return the validated stage/packages."""
    run([npm, "run", "build", "--", "--outDir", stage / "public"], cwd=FRONTEND)
    freeze_env = os.environ.copy()
    freeze_env["PYINSTALLER_CONFIG_DIR"] = str(WORK / "pyinstaller-cache")
    checked_path(WORK / "pyinstaller-cache", WORK)
    run(pyinstaller_command(python, stage), env=freeze_env)
    frozen = stage / "frozen" / f"{APPLICATION}.exe"
    checked_path(frozen, stage)
    frozen.rename(stage / "server.exe")
    # Validate both portable folders rather than PyInstaller's work/spec files.
    artifacts = prepare_artifacts(stage)
    for profile, host in PROFILES.items():
        validate_artifacts(artifacts / profile, host=host)
        run([artifacts / profile / "server.exe", "--help"])
    return artifacts


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--skip-install", action="store_true",
        help="Reuse installed Python/npm dependencies instead of installing them",
    )
    options = parser.parse_args(argv)
    if os.name != "nt":
        raise BuildError("Build the Windows executable on Windows")
    if sys.version_info < (3, 10):
        raise BuildError("Python 3.10 or newer is required")
    validate_port_defaults(PORT_MAPPING)
    npm = shutil.which("npm.cmd")
    if npm is None:
        raise BuildError("Node.js and npm were not found on PATH")
    if not (BACKEND / "packaged.py").is_file():
        raise BuildError("Backend packaged entrypoint is missing")
    checked_path(WORK, ROOT)
    WORK.mkdir(exist_ok=True)
    remove_stale_stages(WORK)
    python = select_python()
    if not options.skip_install:
        run([python, "-m", "pip", "install", "--cache-dir", WORK / "pip-cache",
             "-r", BACKEND / "requirements.txt",
             "-r", BACKEND / "requirements-build.txt"])
        run([npm, "ci", "--cache", WORK / "npm-cache"], cwd=FRONTEND)
    run([npm, "run", "typecheck"], cwd=FRONTEND)

    stage = staging_directory(WORK, OUTPUT)
    stage.mkdir()
    published = False
    try:
        publish_artifacts(build_packages(stage, python=python, npm=npm))
        published = True
    finally:
        if published:
            # staging_directory() keeps this inside build/ and away from build/dist. The new
            # packages are in place: a file that cannot be removed now is removed next time.
            if not remove_build_files(stage, WORK):
                print(f"[build] Warning: could not remove all build files in {stage}; "
                      "the next build removes them.")
        else:
            print(f"[build] Build files retained for troubleshooting until the next build: {stage}")

    relative = OUTPUT.relative_to(ROOT)
    print(f"\n[build] Ready: {OUTPUT / 'Debug'} (local) and {OUTPUT / 'Release'} (LAN)")
    print(f"[build] Local: {relative / 'Debug' / 'run.bat'}")
    print(f"[build] LAN:   {relative / 'Release' / 'run.bat'}")
    print(f"[build] Split: {relative / 'Release' / 'run-backend.bat'} on one machine and "
          "run-frontend.bat --backend-url http://<backend-ip>:<port> on another")
    print("[build] Ports: settings.json; missing values are filled from port_mapping.json")
    if LEGACY_OUTPUT.is_dir():
        print(f"[build] Note: {LEGACY_OUTPUT} is from an older build and is no longer updated; "
              f"move any .env you keep there into {OUTPUT}\\<profile>.")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (BuildError, OSError) as error:
        print(f"\n[build] ERROR: {error}", file=sys.stderr)
        raise SystemExit(1)
