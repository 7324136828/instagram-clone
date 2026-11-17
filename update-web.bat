@echo off
setlocal
cd /d "%~dp0"

rem Rebuilds the browser pages embedded in the native Windows app
rem (windows-app\web\dist) from this repository's frontend sources.
call "%~dp0windows-app\update-web.bat" %*
exit /b %ERRORLEVEL%
