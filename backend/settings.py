"""Per-user application settings and storage paths."""

from __future__ import annotations

import json
import os
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

APP_NAME = "instagram-clone"


class SettingsError(RuntimeError):
    """Raised when the user's settings cannot be read or created."""


@dataclass(frozen=True)
class AppSettings:
    settings_path: Path
    data_directory: Path


def settings_path() -> Path:
    if os.name == "nt":
        root = Path(os.environ.get("APPDATA") or Path.home() / "AppData" / "Roaming")
    else:
        root = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config")
    return root.expanduser().resolve() / APP_NAME / "settings.json"


def default_data_directory() -> Path:
    root = Path(os.environ.get("TEMP") or tempfile.gettempdir())
    return root.expanduser().resolve() / APP_NAME


def _write_json(path: Path, values: dict[str, Any], *, overwrite: bool = False) -> None:
    # Publish complete JSON so another startup never reads a partial file.
    temporary_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w", encoding="utf-8", dir=path.parent,
            prefix=".settings-", suffix=".tmp", delete=False,
        ) as settings_file:
            temporary_path = Path(settings_file.name)
            json.dump(values, settings_file, indent=2)
            settings_file.write("\n")
        if overwrite:
            temporary_path.replace(path)
            return
        try:
            if os.name == "nt":
                # Windows rename fails if the destination already exists.
                temporary_path.rename(path)
            else:
                os.link(temporary_path, path)
        except FileExistsError:
            pass
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)


def _create_default_settings(path: Path, data_directory: Path) -> None:
    _write_json(path, {"data_directory": str(data_directory)})


# On Windows, opening a file that another thread or process is renaming into place (or that a
# virus scanner holds for a moment) fails with PermissionError: such a read is retried briefly.
_READ_ATTEMPTS = 20
_READ_RETRY_SECONDS = 0.025


def _read_text(path: Path) -> str:
    retries = _READ_ATTEMPTS - 1 if os.name == "nt" else 0
    for _ in range(retries):
        try:
            return path.read_text(encoding="utf-8-sig")
        except PermissionError:
            time.sleep(_READ_RETRY_SECONDS)
    return path.read_text(encoding="utf-8-sig")


def _read_json_object(path: Path) -> dict[str, Any]:
    try:
        values = json.loads(_read_text(path))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise SettingsError(f"Cannot read configuration at {path}: {error}") from error
    if not isinstance(values, dict):
        raise SettingsError(f"Invalid configuration at {path}: expected a JSON object")
    return values


def _validate_ports(values: dict[str, Any], path: Path) -> dict[str, int]:
    ports: dict[str, int] = {}
    for field in ("frontend_port", "backend_port"):
        if field not in values:
            raise SettingsError(f"Invalid configuration at {path}: missing {field}")
        value = values[field]
        if type(value) is not int or not 1 <= value <= 65535:
            raise SettingsError(
                f"Invalid configuration at {path}: {field} must be an integer from 1 to 65535"
            )
        ports[field] = value
    if ports["frontend_port"] == ports["backend_port"]:
        raise SettingsError(
            f"Invalid configuration at {path}: frontend_port and backend_port must be different"
        )
    return ports


def load_port_mapping(path: Path) -> dict[str, int]:
    """Read and validate the project's default frontend and backend ports."""
    return _validate_ports(_read_json_object(path), path)


def initialize_port_settings(defaults_path: Path) -> dict[str, int]:
    """Persist missing port defaults, then read the effective per-user ports."""
    defaults = load_port_mapping(defaults_path)
    path = settings_path()
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        if not path.exists():
            _write_json(path, {})
    except OSError as error:
        raise SettingsError(f"Cannot create settings at {path}: {error}") from error

    values = _read_json_object(path)
    missing = [field for field in defaults if field not in values]
    for field in missing:
        values[field] = defaults[field]
    # Validate before updating so invalid existing ports are never replaced.
    _validate_ports(values, path)
    if missing:
        try:
            _write_json(path, values, overwrite=True)
        except OSError as error:
            raise SettingsError(f"Cannot update port settings at {path}: {error}") from error
    return _validate_ports(_read_json_object(path), path)


def load_settings() -> AppSettings:
    """Create defaults once; preserve settings the user has already written."""
    path = settings_path()
    default_directory = default_data_directory()
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        if not path.exists():
            _create_default_settings(path, default_directory)
        settings = json.loads(_read_text(path))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise SettingsError(f"Cannot read or create settings at {path}: {error}") from error

    if not isinstance(settings, dict):
        raise SettingsError(f"Invalid settings at {path}: expected a JSON object")
    configured = settings.get("data_directory", str(default_directory))
    if not isinstance(configured, str) or not configured.strip() or "\0" in configured:
        raise SettingsError(
            f"Invalid settings at {path}: data_directory must be a non-empty folder path"
        )
    try:
        directory = Path(os.path.expandvars(configured)).expanduser()
        if not directory.is_absolute():
            directory = path.parent / directory
        directory = directory.resolve()
    except (OSError, RuntimeError, ValueError) as error:
        raise SettingsError(f"Invalid data_directory in settings at {path}: {error}") from error
    return AppSettings(settings_path=path, data_directory=directory)


CACHE_DIRECTORY_NAME = "instagram_cache"
CACHE_DIRECTORY_ENVIRONMENT = "INSTAGRAM_CACHE_DIR"
CACHE_DIRECTORY_SETTING = "instagram_cache_directory"


def default_cache_directory() -> Path:
    root = Path(os.environ.get("TEMP") or tempfile.gettempdir())
    return root.expanduser().resolve() / CACHE_DIRECTORY_NAME


def _expanded_directory(value: str, relative_to: Path) -> Path:
    directory = Path(os.path.expandvars(value)).expanduser()
    if not directory.is_absolute():
        directory = relative_to / directory
    return directory.resolve()


def configured_cache_directory(base_directory: Path) -> Path:
    """Folder of the merged import library.

    INSTAGRAM_CACHE_DIR wins (relative to base_directory), then the optional settings key
    instagram_cache_directory (relative to the settings folder), then %TEMP%/instagram_cache.
    Unlike load_settings this never creates settings.json: the library is optional.
    """
    override = os.environ.get(CACHE_DIRECTORY_ENVIRONMENT)
    if override and override.strip():
        try:
            return _expanded_directory(override.strip(), base_directory)
        except (OSError, RuntimeError, ValueError) as error:
            raise SettingsError(
                f"Invalid {CACHE_DIRECTORY_ENVIRONMENT} {override!r}: {error}"
            ) from error

    path = settings_path()
    if not path.exists():
        return default_cache_directory()
    configured = _read_json_object(path).get(CACHE_DIRECTORY_SETTING)
    if configured is None:
        return default_cache_directory()
    if not isinstance(configured, str) or not configured.strip() or "\0" in configured:
        raise SettingsError(
            f"Invalid settings at {path}: {CACHE_DIRECTORY_SETTING} must be a non-empty folder path"
        )
    try:
        return _expanded_directory(configured.strip(), path.parent)
    except (OSError, RuntimeError, ValueError) as error:
        raise SettingsError(
            f"Invalid {CACHE_DIRECTORY_SETTING} in settings at {path}: {error}"
        ) from error


ALLOWED_HOSTS_ENVIRONMENT = "ALLOWED_HOSTS"
ALLOWED_HOSTS_SETTING = "allowed_hosts"


def _host_list(value: object, where: str) -> list[str]:
    if isinstance(value, str):
        items: list[object] = list(value.split(","))
    elif isinstance(value, list):
        items = value
    else:
        raise SettingsError(f"Invalid {where}: expected a list of host names")
    if not all(isinstance(item, str) for item in items):
        raise SettingsError(f"Invalid {where}: every host name must be a string")
    return [str(item).strip() for item in items if str(item).strip()]


def configured_allowed_hosts() -> list[str]:
    """Extra host names the app answers to (beyond IP addresses, localhost and this computer's
    names): ALLOWED_HOSTS (comma-separated) plus settings.json "allowed_hosts"; "*" allows any."""
    hosts = _host_list(os.environ.get(ALLOWED_HOSTS_ENVIRONMENT, ""), ALLOWED_HOSTS_ENVIRONMENT)
    path = settings_path()
    if path.exists():
        configured = _read_json_object(path).get(ALLOWED_HOSTS_SETTING)
        if configured is not None:
            hosts += _host_list(configured, f"{ALLOWED_HOSTS_SETTING} in settings at {path}")
    return hosts


def configured_database_path(base_directory: Path) -> Path:
    settings = load_settings()
    override = os.environ.get("COUNTER_DB_PATH")
    if override and override.strip():
        database = Path(os.path.expandvars(override)).expanduser()
        if not database.is_absolute():
            database = base_directory / database
        return database.resolve()
    return settings.data_directory / "counter.db"
