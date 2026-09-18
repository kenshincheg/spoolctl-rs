@echo off
REM Windows 7 x64 — smoke ядра (status + help). GUI не проверяется.
setlocal
cd /d "%~dp0"
if not exist "SpoolCtl.exe" (
  echo [ERROR] SpoolCtl.exe не найден.
  exit /b 1
)
echo === SpoolCtl smoke для Windows 7 ===
echo.
SpoolCtl.exe status
if errorlevel 1 exit /b 1
echo.
SpoolCtl.exe help
if errorlevel 1 exit /b 1
echo.
echo [OK] CLI на Win7. Для перезапуска: restart.cmd или fix.cmd (от администратора).
pause
endlocal
exit /b 0
