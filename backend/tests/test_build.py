from __future__ import annotations

import io
import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from contextlib import ExitStack, redirect_stdout
from pathlib import Path
from unittest import mock

import build
from build import (
    ARTIFACTS,
    LAUNCHER,
    LAUNCHERS,
    PROFILES,
    BuildError,
    checked_path,
    main,
    prepare_artifacts,
    publish_artifacts,
    readme,
    staging_directory,
    validate_artifacts,
)


def write_packages(directory: Path, version: str) -> None:
    for profile, host in PROFILES.items():
        package = directory / profile
        package.mkdir(parents=True, exist_ok=True)
        (package / "server.exe").write_bytes(f"server-{profile}-{version}".encode("ascii"))
        (package / "public" / "assets").mkdir(parents=True)
        (package / "public" / "index.html").write_text(version, encoding="utf-8")
        (package / "public" / "assets" / "app.js").write_text(
            f"script-{profile}-{version}", encoding="utf-8",
        )
        for name in LAUNCHERS:
            (package / name).write_text(f"{name}-{profile}-{version}", encoding="ascii")
        (package / "README.txt").write_text(f"readme-{profile}-{version}", encoding="ascii")
        # Different old defaults make runtime.json replacement and rollback observable.
        runtime_host = host if version == "new" else f"old-{host}"
        (package / "runtime.json").write_text(
            json.dumps({"host": runtime_host}), encoding="ascii",
        )
        ports = {"frontend_port": 4100, "backend_port": 9200}
        if version != "new":
            ports = {"frontend_port": 4300, "backend_port": 9400}
        (package / "port_mapping.json").write_text(json.dumps(ports), encoding="ascii")


def package_contents(directory: Path) -> dict[Path, bytes]:
    return {
        path.relative_to(directory): path.read_bytes()
        for path in directory.rglob("*") if path.is_file()
    }


def managed_contents(directory: Path) -> dict[str, dict[Path, bytes]]:
    return {profile: package_contents(directory / profile) for profile in PROFILES}


def make_stage(stage: Path) -> Path:
    (stage / "server.exe").write_bytes(b"shared backend")
    (stage / "public" / "assets").mkdir(parents=True)
    (stage / "public" / "index.html").write_text("shared frontend", encoding="utf-8")
    (stage / "public" / "assets" / "app.js").write_bytes(b"shared script")
    defaults = stage / "defaults.json"
    defaults.write_text('{"frontend_port": 4100, "backend_port": 9200}\n', encoding="utf-8")
    return defaults


class ArtifactPreparationTests(unittest.TestCase):
    def test_build_creates_two_self_contained_packages_with_mode_defaults(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            stage = Path(directory)
            defaults = make_stage(stage)

            packages = prepare_artifacts(stage, defaults_path=defaults)

            self.assertEqual(packages, stage / "packages")
            self.assertEqual({path.name for path in packages.iterdir()}, {"Debug", "Release"})
            for profile, expected_host in {"Debug": "127.0.0.1", "Release": "0.0.0.0"}.items():
                with self.subTest(profile=profile):
                    package = packages / profile
                    validate_artifacts(package, host=expected_host)
                    self.assertEqual({path.name for path in package.iterdir()}, set(ARTIFACTS))
                    self.assertEqual((package / "server.exe").read_bytes(), b"shared backend")
                    self.assertEqual(package_contents(package / "public"), package_contents(stage / "public"))
                    for name, launcher in LAUNCHERS.items():
                        # Batch files need CRLF line endings.
                        self.assertEqual((package / name).read_bytes(),
                                         launcher.replace("\n", "\r\n").encode("ascii"))
                    self.assertEqual((package / "port_mapping.json").read_bytes(), defaults.read_bytes())
                    self.assertEqual(
                        json.loads((package / "runtime.json").read_text(encoding="ascii")),
                        {"host": expected_host},
                    )
                    self.assertEqual(
                        (package / "README.txt").read_bytes(),
                        readme(profile, {"frontend_port": 4100, "backend_port": 9200})
                        .replace("\n", "\r\n").encode("ascii"),
                    )
                    self.assertFalse(any(path.is_symlink() for path in package.rglob("*")))

            # Each mode remains usable after the shared staging files or the other mode change.
            (stage / "server.exe").unlink()
            defaults.unlink()
            (packages / "Debug" / "public" / "index.html").write_text("changed", encoding="utf-8")
            self.assertEqual((packages / "Release" / "server.exe").read_bytes(), b"shared backend")
            self.assertEqual(
                (packages / "Release" / "public" / "index.html").read_text(encoding="utf-8"),
                "shared frontend",
            )
            self.assertEqual(
                json.loads((packages / "Release" / "port_mapping.json").read_text(encoding="utf-8")),
                {"frontend_port": 4100, "backend_port": 9200},
            )

    def test_launcher_initializes_settings_before_starting_servers(self) -> None:
        initialize = '"%~dp0server.exe" --initialize-settings\n'
        abort_on_failure = "if errorlevel 1 exit /b %ERRORLEVEL%\n"
        launch = ':launch\n"%~dp0server.exe" %*\n'
        self.assertIn(initialize + abort_on_failure + launch, LAUNCHER)
        for flag in ("--help", "-h"):
            help_bypass = f'if /i "%~1"=="{flag}" if "%~2"=="" goto launch\n'
            self.assertLess(LAUNCHER.index(help_bypass), LAUNCHER.index(initialize))

    def test_role_launchers_start_server_exe_with_their_role_and_all_arguments(self) -> None:
        self.assertEqual(set(LAUNCHERS), {"run.bat", "run-frontend.bat", "run-backend.bat"})
        self.assertIs(LAUNCHERS["run.bat"], LAUNCHER)
        for role in ("frontend", "backend"):
            with self.subTest(role=role):
                launcher = LAUNCHERS[f"run-{role}.bat"]
                # "call run.bat" would expand a % in the folder path a second time.
                self.assertNotIn("call ", launcher)
                self.assertIn(f':launch\n"%~dp0server.exe" --role {role} %*\nexit /b %ERRORLEVEL%\n', launcher)
                self.assertEqual(
                    launcher.replace(f"--role {role} ", "").replace(
                        f"rem Runs only the {role} of the application. Extra arguments are passed to server.exe.\n", ""
                    ),
                    LAUNCHER,
                )
                self.assertTrue(launcher.isascii())

    def test_every_launcher_refuses_a_folder_path_with_a_percent_sign_first(self) -> None:
        guard = 'if not "!HERE:%%=!"=="!HERE!" ('
        for name, launcher in LAUNCHERS.items():
            with self.subTest(launcher=name):
                self.assertIn('set "HERE=%~dp0"\nsetlocal EnableDelayedExpansion\n' + guard, launcher)
                self.assertLess(launcher.index(guard), launcher.index('"%~dp0server.exe"'))

    @unittest.skipUnless(os.name == "nt", "batch files run on Windows only")
    def test_launchers_stop_before_server_exe_in_a_folder_with_a_percent_sign(self) -> None:
        # The one-file server.exe started from such a path starts copies of itself without end;
        # a stand-in that is no program at all shows that it is never started.
        with tempfile.TemporaryDirectory() as directory:
            for folder_name, refused in (("pct %OS% dir", True), ("pct 100% x", True), ("odd (copy) & ! name", False)):
                folder = Path(directory) / folder_name
                folder.mkdir()
                (folder / "server.exe").write_bytes(b"not a program")
                for name, launcher in LAUNCHERS.items():
                    (folder / name).write_bytes(launcher.replace("\n", "\r\n").encode("ascii"))
                    with self.subTest(folder=folder_name, launcher=name):
                        result = subprocess.run(
                            ["cmd", "/d", "/c", ".\\" + name, "--help"], cwd=folder,
                            capture_output=True, text=True, timeout=60,
                        )
                        message = "contains a % sign"
                        self.assertNotEqual(result.returncode, 0)
                        if refused:
                            self.assertIn(message, result.stderr)
                        else:
                            self.assertNotIn(message, result.stdout + result.stderr)

    def test_readme_explains_roles_ports_and_the_cache_folder_for_each_profile(self) -> None:
        ports = {"frontend_port": 4100, "backend_port": 9200}
        for profile, host in PROFILES.items():
            with self.subTest(profile=profile):
                text = readme(profile, ports)
                self.assertTrue(text.isascii())
                for expected in (
                    f"portable {profile} build", host, "run.bat", "run-backend.bat",
                    "run-frontend.bat --backend-url http://192.168.1.20:9200",
                    "frontend 4100", "backend 9200", "port_mapping.json",
                    r"%APPDATA%\instagram-clone\settings.json", '"backend_url"',
                    r"%TEMP%\instagram_cache", "INSTAGRAM_CACHE_DIR", "instagram_cache_directory",
                    "folder of its", "never deletes files it did not write", '"allowed_hosts"',
                    "must not contain a % sign",
                ):
                    self.assertIn(expected, text)
                if profile == "Release":
                    self.assertIn("There is no password", text)
                    self.assertIn("use the Debug build", text)
                self.assertNotRegex(text, r"[{}]")

    def test_preparation_rejects_missing_or_invalid_port_defaults_before_creating_packages(self) -> None:
        for contents in (None, "{}", "not json", '{"frontend_port": 0, "backend_port": 9200}'):
            with self.subTest(contents=contents), tempfile.TemporaryDirectory() as directory:
                stage = Path(directory)
                defaults = stage / "defaults.json"
                if contents is not None:
                    defaults.write_text(contents, encoding="utf-8")
                with self.assertRaisesRegex(BuildError, "Invalid port defaults"):
                    prepare_artifacts(stage, defaults_path=defaults)
                self.assertFalse((stage / "packages").exists())

    def test_build_rejects_invalid_port_defaults_before_installing_dependencies(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            missing_defaults = Path(directory) / "missing.json"
            with (
                mock.patch("build.os.name", "nt"),
                mock.patch("build.PORT_MAPPING", missing_defaults),
                mock.patch("build.run") as run,
                mock.patch("build.select_python") as select_python,
            ):
                with self.assertRaisesRegex(BuildError, "Invalid port defaults"):
                    main([])
                run.assert_not_called()
                select_python.assert_not_called()

    def test_validation_rejects_an_incorrect_runtime_default(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            stage = Path(directory)
            packages = prepare_artifacts(stage, defaults_path=make_stage(stage))
            with self.assertRaisesRegex(BuildError, "Unexpected runtime defaults"):
                validate_artifacts(packages / "Debug", host="0.0.0.0")

    def test_validation_rejects_missing_or_invalid_packaged_port_defaults(self) -> None:
        for contents in (None, "{}", "[]", '{"frontend_port": 4100, "backend_port": "9200"}'):
            with self.subTest(contents=contents), tempfile.TemporaryDirectory() as directory:
                stage = Path(directory)
                packages = prepare_artifacts(stage, defaults_path=make_stage(stage))
                defaults = packages / "Debug" / "port_mapping.json"
                if contents is None:
                    defaults.unlink()
                else:
                    defaults.write_text(contents, encoding="utf-8")
                with self.assertRaisesRegex(BuildError, "Invalid port defaults"):
                    validate_artifacts(packages / "Debug", host=PROFILES["Debug"])

    def test_validation_rejects_missing_changed_or_extra_launchers_and_missing_readme(self) -> None:
        def missing(package: Path, name: str) -> None:
            (package / name).unlink()

        def changed(package: Path, name: str) -> None:
            (package / name).write_text("@echo off\r\n", encoding="ascii")

        cases = [
            *((f"missing {name}", name, missing, "Unexpected batch files") for name in LAUNCHERS),
            *((f"changed {name}", name, changed, "Unexpected launcher content") for name in LAUNCHERS),
            ("extra launcher", "public/tools.bat", changed, "Unexpected batch files"),
            ("missing README", "README.txt", missing, "Missing package README"),
            ("empty README", "README.txt", lambda package, name: (package / name).write_text(" "),
             "Missing package README"),
        ]
        for label, name, damage, message in cases:
            with self.subTest(case=label), tempfile.TemporaryDirectory() as directory:
                stage = Path(directory)
                package = prepare_artifacts(stage, defaults_path=make_stage(stage)) / "Release"
                damage(package, name)
                with self.assertRaisesRegex(BuildError, message):
                    validate_artifacts(package, host=PROFILES["Release"])


class PublicationTests(unittest.TestCase):
    def layout(self, directory: str) -> tuple[Path, Path, Path]:
        repository = Path(directory)
        return repository, repository / "build" / "dist", repository / "build" / "packages"

    def test_output_is_build_dist_and_work_files_stay_in_build(self) -> None:
        self.assertEqual(build.OUTPUT, build.ROOT / "build" / "dist")
        self.assertEqual(build.WORK, build.ROOT / "build")
        stage = staging_directory(build.WORK, build.OUTPUT)
        self.assertEqual(stage.parent, build.WORK)
        self.assertRegex(stage.name, r"^portable-[0-9a-f]{32}$")
        self.assertNotEqual(stage, build.OUTPUT)

    def test_staging_directory_never_overlaps_the_output(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory) / "build"
            work.mkdir()
            self.assertEqual(staging_directory(work, work / "dist").parent, work)
            for output in (work, work / "portable-x" / "dist"):
                with (
                    self.subTest(output=output),
                    mock.patch("build.uuid.uuid4", return_value=mock.Mock(hex="x")),
                    self.assertRaisesRegex(BuildError, "overlaps the output"),
                ):
                    staging_directory(work, output)

    def test_replacement_preserves_each_modes_database_and_local_configuration(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            repository, output, stage = self.layout(directory)
            write_packages(output, "old")
            write_packages(stage, "new")
            old_artifacts = managed_contents(output)
            new_artifacts = managed_contents(stage)
            user_state = {}
            for profile in PROFILES:
                package = output / profile
                (package / "data").mkdir()
                (package / "data" / "counter.db").write_bytes(f"{profile} counter data".encode("ascii"))
                (package / ".env").write_text(f"BACKEND_PORT=9000\nMODE={profile}\n", encoding="utf-8")
                (package / "notes.txt").write_text(f"{profile} user notes", encoding="utf-8")
                user_state[profile] = {
                    name: (package / name).read_bytes()
                    for name in ("data/counter.db", ".env", "notes.txt")
                }

            publish_artifacts(stage, repository=repository)

            for profile in PROFILES:
                with self.subTest(profile=profile):
                    actual = package_contents(output / profile)
                    self.assertEqual(
                        {path: content for path, content in actual.items() if path.parts[0] in ARTIFACTS},
                        new_artifacts[profile],
                    )
                    for name, content in user_state[profile].items():
                        self.assertEqual((output / profile / name).read_bytes(), content)
                    self.assertEqual(package_contents(stage / "previous" / profile), old_artifacts[profile])
                    self.assertFalse(any((stage / profile / name).exists() for name in ARTIFACTS))

    def test_profile_from_an_older_build_gains_the_new_files(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            repository, output, stage = self.layout(directory)
            write_packages(output, "old")
            for profile in PROFILES:
                for name in ("run-frontend.bat", "run-backend.bat", "README.txt"):
                    (output / profile / name).unlink()
                (output / profile / ".env").write_text("MODE=kept", encoding="utf-8")
            write_packages(stage, "new")
            new_artifacts = managed_contents(stage)

            publish_artifacts(stage, repository=repository)

            for profile in PROFILES:
                actual = package_contents(output / profile)
                self.assertEqual(actual.pop(Path(".env")), b"MODE=kept")
                self.assertEqual(actual, new_artifacts[profile])

    def test_locked_release_destination_rolls_back_both_existing_modes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            repository, output, stage = self.layout(directory)
            write_packages(output, "old")
            write_packages(stage, "new")
            for profile in PROFILES:
                (output / profile / ".env").write_text(f"MODE={profile}", encoding="utf-8")
            old_contents = managed_contents(output)
            new_contents = managed_contents(stage)
            original_rename = Path.rename
            moves = []

            def fail_locked_release(source: Path, target: Path) -> Path:
                if source == output / "Release" / "public":
                    raise OSError("simulated locked Release destination")
                moves.append((source, Path(target)))
                return original_rename(source, target)

            with mock.patch.object(Path, "rename", new=fail_locked_release):
                with self.assertRaisesRegex(BuildError, "Could not replace Debug/Release artifacts"):
                    publish_artifacts(stage, repository=repository)

            for name in ARTIFACTS:
                self.assertIn((stage / "Debug" / name, output / "Debug" / name), moves)
            self.assertIn((stage / "Release" / "server.exe", output / "Release" / "server.exe"), moves)
            self.assertEqual(managed_contents(output), old_contents)
            self.assertEqual(managed_contents(stage), new_contents)

    def test_failed_first_install_leaves_no_mixed_artifacts(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            repository, output, stage = self.layout(directory)
            write_packages(stage, "new")
            new_contents = managed_contents(stage)
            original_rename = Path.rename

            def fail_release_install(source: Path, target: Path) -> Path:
                if source == stage / "Release" / "public":
                    raise OSError("simulated failed first installation")
                return original_rename(source, target)

            with mock.patch.object(Path, "rename", new=fail_release_install):
                with self.assertRaises(BuildError):
                    publish_artifacts(stage, repository=repository)

            self.assertEqual(managed_contents(stage), new_contents)
            for profile in PROFILES:
                with self.subTest(profile=profile):
                    self.assertEqual(package_contents(output / profile), {})
                    self.assertFalse(any((output / profile / name).exists() for name in ARTIFACTS))

    def test_a_file_in_use_is_reported_before_either_mode_is_moved(self) -> None:
        # Windows renames a running server.exe without complaint but refuses to open it for
        # writing, so the build must ask before it moves anything.
        with tempfile.TemporaryDirectory() as directory:
            repository, output, stage = self.layout(directory)
            write_packages(output, "old")
            write_packages(stage, "new")
            old_contents = managed_contents(output)
            new_contents = managed_contents(stage)
            running = output / "Release" / "server.exe"
            real_open = os.open

            def open_file(path: object, flags: int, *args: object) -> int:
                if Path(str(path)) == running and flags & os.O_RDWR:
                    raise PermissionError(13, "Permission denied", str(path))
                return real_open(path, flags, *args)  # type: ignore[arg-type]

            with (
                mock.patch("build.os.open", side_effect=open_file),
                mock.patch.object(Path, "rename") as rename,
            ):
                with self.assertRaisesRegex(BuildError, r"server\.exe is in use.*Close it and retry"):
                    publish_artifacts(stage, repository=repository)
                rename.assert_not_called()
            self.assertEqual(managed_contents(output), old_contents)
            self.assertEqual(managed_contents(stage), new_contents)

    @unittest.skipUnless(os.name == "nt", "Windows locks running executables")
    def test_a_running_server_exe_is_detected(self) -> None:
        source = Path(os.environ.get("SystemRoot", r"C:\Windows")) / "System32" / "PING.EXE"
        if not source.is_file():
            self.skipTest("PING.EXE is not available")
        with tempfile.TemporaryDirectory() as directory:
            executable = Path(directory) / "server.exe"
            shutil.copy2(source, executable)
            build.ensure_replaceable(executable)  # not running: fine
            process = subprocess.Popen(
                [str(executable), "-n", "30", "127.0.0.1"],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
            try:
                with self.assertRaisesRegex(BuildError, "is in use"):
                    build.ensure_replaceable(executable)
            finally:
                process.kill()
                process.wait(timeout=10)
            build.ensure_replaceable(executable)

    def test_unsafe_release_target_is_rejected_before_either_mode_is_moved(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            repository, output, stage = self.layout(directory)
            write_packages(output, "old")
            write_packages(stage, "new")
            old_contents = managed_contents(output)
            new_contents = managed_contents(stage)
            original_resolve = Path.resolve

            def resolve_unsafe_target(path: Path, *args: object, **kwargs: object) -> Path:
                if path == output / "Release" / "public":
                    return repository / "outside"
                return original_resolve(path, *args, **kwargs)

            with (
                mock.patch.object(Path, "resolve", new=resolve_unsafe_target),
                mock.patch.object(Path, "rename") as rename,
            ):
                with self.assertRaisesRegex(BuildError, "Unsafe build path"):
                    publish_artifacts(stage, repository=repository)
                rename.assert_not_called()

            self.assertEqual(managed_contents(output), old_contents)
            self.assertEqual(managed_contents(stage), new_contents)

    def test_staging_inside_the_output_or_outside_build_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            repository, output, _ = self.layout(directory)
            write_packages(output, "old")
            old_contents = managed_contents(output)
            for stage, message in (
                (output / "packages", "must not be inside the output"),
                (repository / "packages", "Unsafe build path"),
            ):
                with self.subTest(stage=stage):
                    write_packages(stage, "new")
                    with mock.patch.object(Path, "rename") as rename:
                        with self.assertRaisesRegex(BuildError, message):
                            publish_artifacts(stage, repository=repository)
                        rename.assert_not_called()
            self.assertEqual(managed_contents(output), old_contents)

    def test_checked_path_rejects_parent_and_paths_escaping_the_workspace(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            repository = Path(directory) / "repository"
            repository.mkdir()
            for unsafe in (repository, repository / ".." / "outside"):
                with self.subTest(path=unsafe), self.assertRaises(BuildError):
                    checked_path(unsafe, repository)


class BuildCommandTests(unittest.TestCase):
    def run_main(self, repository: Path, build_packages: mock.Mock) -> str:
        work = repository / "build"
        real_publish = publish_artifacts
        output = io.StringIO()
        with ExitStack() as stack:
            for name, value in (
                ("ROOT", repository), ("WORK", work), ("OUTPUT", work / "dist"),
                ("LEGACY_OUTPUT", repository / "dist"),
            ):
                stack.enter_context(mock.patch(f"build.{name}", value))
            stack.enter_context(mock.patch("build.shutil.which", return_value="npm.cmd"))
            stack.enter_context(mock.patch("build.select_python", return_value=Path("python.exe")))
            stack.enter_context(mock.patch("build.run"))
            stack.enter_context(mock.patch("build.build_packages", build_packages))
            stack.enter_context(mock.patch(
                "build.publish_artifacts",
                side_effect=lambda stage: real_publish(stage, repository=repository),
            ))
            stack.enter_context(redirect_stdout(output))
            self.assertEqual(main(["--skip-install"]), 0)
        return output.getvalue()

    def user_files(self, output: Path) -> dict[Path, bytes]:
        for profile in PROFILES:
            (output / profile).mkdir(parents=True, exist_ok=True)
            (output / profile / ".env").write_text(f"MODE={profile}", encoding="utf-8")
            (output / profile / "data").mkdir(exist_ok=True)
            (output / profile / "data" / "counter.db").write_bytes(b"user data")
        return {path: path.read_bytes() for path in output.rglob("*") if path.is_file()}

    def test_successful_build_removes_only_its_staging_folder(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            repository = Path(directory)
            output = repository / "build" / "dist"
            write_packages(output, "old")
            user = {path: content for path, content in self.user_files(output).items()
                    if path.name in (".env", "counter.db")}
            (repository / "build" / "npm-cache").mkdir()

            def build_packages(stage: Path, *, python: Path, npm: str) -> Path:
                self.assertEqual(stage.parent, repository / "build")
                write_packages(stage / "packages", "new")
                return stage / "packages"

            printed = self.run_main(repository, mock.Mock(side_effect=build_packages))

            for path, content in user.items():
                self.assertEqual(path.read_bytes(), content)
            self.assertEqual((output / "Release" / "server.exe").read_bytes(), b"server-Release-new")
            self.assertEqual(sorted(path.name for path in (repository / "build").iterdir()),
                             ["dist", "npm-cache"])
            self.assertIn(str(Path("build") / "dist" / "Release" / "run.bat"), printed)
            self.assertIn("run-backend.bat", printed)
            self.assertNotIn("is from an older build", printed)

    def test_build_files_of_earlier_builds_are_removed_and_a_locked_one_does_not_fail_the_build(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            repository = Path(directory)
            work = repository / "build"
            older = work / ("portable-" + "a" * 32)
            (older / "packages").mkdir(parents=True)
            (older / "packages" / "server.exe").write_bytes(b"old")
            unrelated = work / "portable-notes"
            unrelated.mkdir()
            stages: list[Path] = []

            def build_packages(stage: Path, *, python: Path, npm: str) -> Path:
                stages.append(stage)
                write_packages(stage / "packages", "new")
                return stage / "packages"

            real_rmtree = shutil.rmtree

            def rmtree_leaving_the_new_stage(path: object, *args: object, **kwargs: object) -> None:
                if stages and Path(str(path)) == stages[0]:
                    return  # e.g. an old server.exe that is still running from it
                real_rmtree(path, *args, **kwargs)  # type: ignore[arg-type]

            with mock.patch("build.shutil.rmtree", side_effect=rmtree_leaving_the_new_stage):
                printed = self.run_main(repository, mock.Mock(side_effect=build_packages))

            self.assertFalse(older.exists())
            self.assertTrue(unrelated.is_dir())
            self.assertEqual((work / "dist" / "Release" / "server.exe").read_bytes(), b"server-Release-new")
            self.assertIn("could not remove all build files", printed)
            self.assertIn("[build] Ready:", printed)

    def test_failed_build_keeps_its_staging_folder_and_the_published_packages(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            repository = Path(directory)
            output = repository / "build" / "dist"
            write_packages(output, "old")
            before = self.user_files(output)
            failing = mock.Mock(side_effect=BuildError("frontend build failed"))
            with self.assertRaisesRegex(BuildError, "frontend build failed"):
                self.run_main(repository, failing)
            self.assertEqual({path: path.read_bytes() for path in output.rglob("*") if path.is_file()},
                             before)
            retained = [path.name for path in (repository / "build").iterdir() if path.name != "dist"]
            self.assertEqual(len(retained), 1)
            self.assertTrue(re.fullmatch(r"portable-[0-9a-f]{32}", retained[0]))

    def test_output_of_older_builds_is_left_alone_and_mentioned(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            repository = Path(directory)
            legacy = repository / "dist" / "Release"
            legacy.mkdir(parents=True)
            (legacy / ".env").write_text("MODE=legacy", encoding="utf-8")

            def build_packages(stage: Path, *, python: Path, npm: str) -> Path:
                write_packages(stage / "packages", "new")
                return stage / "packages"

            printed = self.run_main(repository, mock.Mock(side_effect=build_packages))
            self.assertIn(f"{repository / 'dist'} is from an older build", printed)
            self.assertEqual((legacy / ".env").read_text(encoding="utf-8"), "MODE=legacy")


if __name__ == "__main__":
    unittest.main()
