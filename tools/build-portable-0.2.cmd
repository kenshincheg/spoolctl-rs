@echo off
REM Portable-сборка SpoolCtl 0.2.x (удалёнка) в отдельную папку — не трогает dist\stage (0.1.15).
setlocal EnableExtensions
cd /d "%~dp0.."

set "VERSION=0.2.0"
set "CARGO_TARGET_DIR=E:\cargo-target\spoolctl-rs-0.2"
set "STAGE=dist\stage-0.2\SpoolCtl-%VERSION%"
set "ZIP=dist\SpoolCtl-%VERSION%-win64.zip"
set "HASH=dist\SpoolCtl-%VERSION%-win64.zip.sha256"
set "WIN7=%STAGE%\Windows7-CLI"

echo === SpoolCtl portable build %VERSION% (remote line) ===
echo target-dir: %CARGO_TARGET_DIR%
echo stage:      %STAGE%
set "CARGO_TARGET_DIR=%CARGO_TARGET_DIR%"
cargo build --release
if errorlevel 1 exit /b 1

if exist "dist\stage-0.2" rmdir /s /q "dist\stage-0.2"
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
if exist "docs\REMOTE.md" copy /Y "docs\REMOTE.md" "%STAGE%\REMOTE.md" >nul

echo Windows 10 / 11: SpoolCtl.exe — GUI; удалённый ПК — поле в шапке или CLI --host.> "%STAGE%\ДЛЯ-WINDOWS-10-11.txt"
echo Стабильная 0.1.15 остаётся в dist\stage\ — эта папка только линия 0.2.>> "%STAGE%\ДЛЯ-WINDOWS-10-11.txt"

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
echo Готово (линия 0.2, не затирает 0.1.15):
echo   %STAGE%\SpoolCtl.exe
echo   %ZIP%
echo   %HASH%
echo   (0.1.15: dist\stage\ и старый ZIP без изменений)
endlocal
exit /b 0
