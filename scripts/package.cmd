@echo off
setlocal EnableExtensions DisableDelayedExpansion
pushd "%~dp0.." || exit /b 1

set "targetDir=%CD%\target"
set "packageDir=%CD%\dist\dragoman-windows"
set "DRAGOMAN_RELEASE_EXE=%targetDir%\release\dragoman.exe"
set "DRAGOMAN_PACKAGE_EXE=%packageDir%\dragoman.exe"

set "cargo=cargo.exe"
where.exe cargo.exe >nul 2>&1
if errorlevel 1 (
    if not exist "%USERPROFILE%\.cargo\bin\cargo.exe" (
        echo Cargo is missing. Install Rust or put Cargo on PATH. 1>&2
        goto :failed
    )
    set "cargo=%USERPROFILE%\.cargo\bin\cargo.exe"
)

rem Match the full executable path so other Dragoman installations stay running.
powershell.exe -NoLogo -NoProfile -NonInteractive -Command "$ErrorActionPreference = 'Stop'; Get-Process | Where-Object { $_.ProcessName -eq 'dragoman' -and $_.Path -in @($env:DRAGOMAN_RELEASE_EXE, $env:DRAGOMAN_PACKAGE_EXE) -and -not $_.HasExited } | ForEach-Object { Write-Host ('Stopping dragoman.exe (PID {0})' -f $_.Id); Stop-Process -InputObject $_; if (-not $_.WaitForExit(10000)) { throw ('Timed out waiting for dragoman.exe (PID {0}) to exit.' -f $_.Id) } }"
if errorlevel 1 goto :failed

"%cargo%" build --release --locked --target-dir "%targetDir%"
if errorlevel 1 goto :failed

if not exist "%packageDir%\" (
    mkdir "%packageDir%" || goto :failed
)
copy /Y "%DRAGOMAN_RELEASE_EXE%" "%packageDir%\dragoman.exe" >nul || goto :failed
"%WINDIR%\Microsoft.NET\Framework64\v4.0.30319\csc.exe" /nologo /warnaserror+ /codepage:65001 /target:winexe /reference:System.Web.Extensions.dll /out:"%packageDir%\DragomanGui.exe" "scripts\windows\DragomanGui.cs"
if errorlevel 1 goto :failed
copy /Y "scripts\windows\login.cmd" "%packageDir%\login.cmd" >nul || goto :failed
copy /Y "scripts\windows\stop-dragoman.ps1" "%packageDir%\stop-dragoman.ps1" >nul || goto :failed
copy /Y "scripts\windows\start.cmd" "%packageDir%\start.cmd" >nul || goto :failed
copy /Y "README.md" "%packageDir%\README.md" >nul || goto :failed
copy /Y "LICENSE" "%packageDir%\LICENSE" >nul || goto :failed

echo Package files copied to "%packageDir%".
popd
exit /b 0

:failed
popd
exit /b 1
