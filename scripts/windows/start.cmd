@echo off
setlocal EnableExtensions DisableDelayedExpansion

set "dragomanExe=%~dp0dragoman.exe"
set "servePort=8080"

:parse_arguments
if "%~1"=="" goto :stop_previous
for /f "tokens=1,* delims==" %%A in ("%~1") do (
    if /I "%%A"=="--port" if not "%%B"=="" set "servePort=%%B"
    if /I "%%A"=="-p" if not "%%B"=="" set "servePort=%%B"
)
if /I "%~1"=="--port" (
    if not "%~2"=="" set "servePort=%~2"
    shift
    shift
    goto :parse_arguments
)
if /I "%~1"=="-p" (
    if not "%~2"=="" set "servePort=%~2"
    shift
    shift
    goto :parse_arguments
)
shift
goto :parse_arguments

:stop_previous
powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "%~dp0stop-dragoman.ps1" -ExecutablePath "%dragomanExe%" -Port %servePort%
if errorlevel 1 (
    echo.
    echo Could not stop the previous Dragoman instance or free the port.
    pause
    exit /b 1
)

"%dragomanExe%" serve %*
set "serveExit=%errorlevel%"
if not "%serveExit%"=="0" (
    echo.
    if "%serveExit%"=="2" (
        echo Dragoman could not bind port %servePort%.
        echo Stop the process using that port or choose another port with --port.
    ) else (
        echo If the session is missing or expired, run login.cmd first.
    )
    pause
)
exit /b %serveExit%
