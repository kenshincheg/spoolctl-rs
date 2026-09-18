@echo off
REM Portable-сборка SpoolCtl: EXE + Win10/11 + папка Windows7-CLI + ZIP + SHA256.
setlocal EnableExtensions
cd /d "%~dp0.."

set "VERSION=0.1.15"
set "CARGO_TARGET_DIR=E:\cargo-target\spoolctl-rs"
set "STAGE=dist\stage\SpoolCtl-%VERSION%"
set "ZIP=dist\SpoolCtl-%VERSION%-win64.zip"
set "HASH=dist\SpoolCtl-%VERSION%-win64.zip.sha256"
set "WIN7=%STAGE%\Windows7-CLI"

echo === SpoolCtl portable build %VERSION% ===
echo target-dir: %CARGO_TARGET_DIR%
cargo build --release
if errorlevel 1 exit /b 1

if exist "dist\stage" rmdir /s /q "dist\stage"
mkdir "%STAGE%"
mkdir "%WIN7%"

copy /Y "%CARGO_TARGET_DIR%\release\spoolctl.exe" "%STAGE%\SpoolCtl.exe" >nul
if errorlevel 1 (
  echo [ERROR] Не найден %CARGO_TARGET_DIR%\release\spoolctl.exe
  exit /b 1
)
copy /Y "README.md" "%STAGE%\README.md" >nul
copy /Y "docs\RELEASE_NOTES.md" "%STAGE%\RELEASE_NOTES.md" >nul
copy /Y "docs\WIN7.md" "%STAGE%\WIN7.md" >nul

REM Корень архива — Windows 10 / 11 (GUI по двойному щелчку).
echo Windows 10 / 11: двойной щелчок по SpoolCtl.exe открывает окно.> "%STAGE%\ДЛЯ-WINDOWS-10-11.txt"
echo Для Windows 7 откройте папку Windows7-CLI\.>> "%STAGE%\ДЛЯ-WINDOWS-10-11.txt"

REM Папка Windows7-CLI — явно для Win7 (только командная строка).
copy /Y "%CARGO_TARGET_DIR%\release\spoolctl.exe" "%WIN7%\SpoolCtl.exe" >nul
copy /Y "tools\win7\ЧИТАЙ-МЕНЯ.txt" "%WIN7%\ЧИТАЙ-МЕНЯ.txt" >nul
copy /Y "tools\win7\status.cmd" "%WIN7%\status.cmd" >nul
copy /Y "tools\win7\restart.cmd" "%WIN7%\restart.cmd" >nul
copy /Y "tools\win7\fix.cmd" "%WIN7%\fix.cmd" >nul
copy /Y "tools\win7\smoke.cmd" "%WIN7%\smoke.cmd" >nul
copy /Y "docs\WIN7.md" "%WIN7%\WIN7.md" >nul

if exist "%ZIP%" del /f /q "%ZIP%"
powershell -NoProfile -Command "Compress-Archive -Path '%STAGE%\*' -DestinationPath '%ZIP%' -Force"
if errorlevel 1 exit /b 1

powershell -NoProfile -Command "$h = Get-FileHash -Algorithm SHA256 -Path '%ZIP%'; ($h.Hash + '  SpoolCtl-%VERSION%-win64.zip') | Set-Content -Encoding ASCII '%HASH%'; Write-Output $h.Hash"

echo.
echo Готово:
echo   %STAGE%\SpoolCtl.exe          ^(Windows 10/11 GUI^)
echo   %WIN7%\                       ^(Windows 7 CLI^)
echo   %ZIP%
echo   %HASH%
endlocal
exit /b 0
