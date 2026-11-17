@echo off
setlocal
cd /d "%~dp0"

rem Builds the whole application for Windows:
rem   1. the portable web packages (build\dist\Debug and build\dist\Release,
rem      server.exe + the built frontend), via build.py;
rem   2. the native Windows app (windows-app\build\dist\Debug and Release,
rem      instagram-windows-app.exe), via windows-app\build.bat.
rem
rem   build.bat [--web-only] [--app-only] [--skip-install] [--skip-tests]
rem             [--profile-only Debug^|Release]

set "DO_WEB=1"
set "DO_APP=1"
set "WEB_ARGS="
set "APP_ARGS="

:parse
if "%~1"=="" goto parsed
if /i "%~1"=="--web-only" (set "DO_APP=" & shift & goto parse)
if /i "%~1"=="--app-only" (set "DO_WEB=" & shift & goto parse)
if /i "%~1"=="--skip-install" (set "WEB_ARGS=%WEB_ARGS% --skip-install" & shift & goto parse)
if /i "%~1"=="--skip-tests" (set "APP_ARGS=%APP_ARGS% --skip-tests" & shift & goto parse)
if /i "%~1"=="--profile-only" (
  if "%~2"=="" (echo [build] ERROR: --profile-only needs Debug or Release. 1>&2 & exit /b 2)
  set "APP_ARGS=%APP_ARGS% --profile-only %~2" & shift & shift & goto parse
)
echo [build] ERROR: Unknown option "%~1". 1>&2
echo Usage: build.bat [--web-only] [--app-only] [--skip-install] [--skip-tests] [--profile-only Debug^|Release] 1>&2
exit /b 2

:parsed
if not defined DO_WEB if not defined DO_APP (
  echo [build] ERROR: --web-only and --app-only together leave nothing to build. 1>&2
  exit /b 2
)

if not defined DO_WEB goto app
if defined VIRTUAL_ENV goto active
if defined CONDA_PREFIX goto active
if exist ".venv\Scripts\python.exe" goto local
where python >nul 2>nul
if errorlevel 1 goto pythonlauncher

:active
python build.py %WEB_ARGS%
if errorlevel 1 exit /b 1
goto app

:local
".venv\Scripts\python.exe" build.py %WEB_ARGS%
if errorlevel 1 exit /b 1
goto app

:pythonlauncher
py -3 build.py %WEB_ARGS%
if errorlevel 1 exit /b 1

:app
if not defined DO_APP goto done
call "%~dp0windows-app\build.bat" %APP_ARGS%
if errorlevel 1 exit /b 1

:done
exit /b 0
