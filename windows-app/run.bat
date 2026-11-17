@echo off
setlocal
cd /d "%~dp0"

if not exist "build\setup.stamp" call "%~dp0setup.bat"
if errorlevel 1 exit /b %ERRORLEVEL%
where cargo >nul 2>nul
if errorlevel 1 set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
cargo run --locked -p instagram-windows-app -- %*
exit /b %ERRORLEVEL%
