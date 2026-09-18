@echo off
REM Windows 7 x64 — перезапуск Spooler. Нужны права администратора.
setlocal
cd /d "%~dp0"
echo [Windows 7] Перезапуск Spooler...
SpoolCtl.exe restart
echo.
pause
endlocal
