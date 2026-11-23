@echo off
setlocal EnableExtensions DisableDelayedExpansion

"%~dp0dragoman.exe" login
set "loginExit=%errorlevel%"
echo.
if "%loginExit%"=="0" (
    echo Session saved. Run start.cmd to start the proxy.
) else (
    echo If Chrome is still signing in, finish in Chrome and run login.cmd again.
)
pause
exit /b %loginExit%
