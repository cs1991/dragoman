@echo off
setlocal EnableExtensions DisableDelayedExpansion

"%~dp0dragoman.exe" login-batch %*
set "batchExit=%errorlevel%"
echo.
if "%batchExit%"=="0" (
    echo Batch login finished. Run start.cmd to use the new accounts.
) else (
    echo Batch login finished with failures. Check batch-login-report.json in the config directory.
)
pause
exit /b %batchExit%
