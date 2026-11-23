@echo off
setlocal EnableExtensions DisableDelayedExpansion

"%~dp0dragoman.exe" serve
set "serveExit=%errorlevel%"
if not "%serveExit%"=="0" (
    echo.
    echo If the session is missing or expired, run login.cmd first.
    pause
)
exit /b %serveExit%
