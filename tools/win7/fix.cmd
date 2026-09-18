@echo off
REM Windows 7 x64 — очистка очереди + перезапуск. Нужны права администратора.
setlocal
cd /d "%~dp0"
echo [Windows 7] Очистка очереди и перезапуск Spooler...
SpoolCtl.exe fix
echo.
pause
endlocal
