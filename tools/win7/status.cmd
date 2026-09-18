@echo off
REM Windows 7 x64 — только CLI (GUI не поддерживается).
setlocal
cd /d "%~dp0"
if not exist "SpoolCtl.exe" (
  echo [ERROR] SpoolCtl.exe не найден в папке Windows7-CLI.
  pause
  exit /b 1
)
SpoolCtl.exe status
echo.
pause
endlocal
