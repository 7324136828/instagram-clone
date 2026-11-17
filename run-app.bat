@echo off
setlocal
cd /d "%~dp0"

rem Starts the native Windows app (windows-app\instagram-windows-app.exe via
rem cargo). Arguments are passed through, for example:
rem   run-app.bat C:\Users\me\Pictures\Holiday
call "%~dp0windows-app\run.bat" %*
exit /b %ERRORLEVEL%
