#Requires -RunAsAdministrator
# Отключение автоматической настройки сетевых устройств (WSD / NcdAutoSetup).
# Аналог кнопки SpoolCtl «Откл. автонастройку».

$ErrorActionPreference = "Stop"

Write-Host "Disabling automatic setup of network-connected devices..."

$root = "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\NcdAutoSetup"
$private = Join-Path $root "Private"

if (-not (Test-Path $private)) {
    New-Item -Path $private -Force | Out-Null
}

# Settings toggle (Private profile) + optional global kill-switch
New-ItemProperty -Path $private -Name "AutoSetup" -PropertyType DWord -Value 0 -Force | Out-Null
New-ItemProperty -Path $root -Name "GlobalAutoSetup" -PropertyType DWord -Value 0 -Force | Out-Null

Write-Host "Registry: Private\AutoSetup = 0, GlobalAutoSetup = 0"

$serviceName = "NcdAutoSetup"
$service = Get-Service -Name $serviceName -ErrorAction SilentlyContinue

if ($service) {
    if ($service.Status -ne "Stopped") {
        Stop-Service -Name $serviceName -Force -ErrorAction SilentlyContinue
    }
    Set-Service -Name $serviceName -StartupType Disabled
    Write-Host "Service $serviceName disabled."
}
else {
    Write-Warning "Service $serviceName not found (OK on some editions)."
}

Write-Host ""
Write-Host "Done. Reboot Windows is recommended."
Write-Host "Already installed printers are kept; only auto-discovery of new ones is stopped."
