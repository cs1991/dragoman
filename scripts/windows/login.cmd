@echo off
setlocal EnableExtensions DisableDelayedExpansion

"%~dp0dragoman.exe" login
set "loginExit=%errorlevel%"
echo.
if "%loginExit%"=="0" (
    echo Session saved. Chrome stays open. Run start.cmd to start the proxy.
) else (
    echo The Chrome window closed before the sign-in finished. Run login.cmd and keep it open.
)
pause
exit /b %loginExit%
