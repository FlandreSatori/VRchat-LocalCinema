@echo off
REM ============================================================
REM  VRchat-LocalCinema - build from source and package portable
REM  Produces dist-portable\VRchat-LocalCinema-v<version>-base.zip
REM          dist-portable\VRchat-LocalCinema-v<version>-full\  (app + components\, folder only, NOT zipped)
REM
REM  Usage:
REM    build.bat
REM
REM  Keep this file ASCII-only: cmd.exe misparses non-ASCII without chcp.
REM ============================================================
setlocal
cd /d "%~dp0"

where npm >nul 2>nul
if errorlevel 1 (
    echo [ERROR] npm not found in PATH. Please install Node.js first.
    pause
    exit /b 1
)

echo ==^> Step 1/2: building frontend + Rust app + portable output via scripts\package-portable.ps1 ...
powershell -NoProfile -ExecutionPolicy Bypass -File "scripts\package-portable.ps1"
set EC=%ERRORLEVEL%
if not "%EC%"=="0" (
    echo.
    echo [ERROR] Packaging failed with exit code %EC%. See messages above.
    pause
    exit /b %EC%
)

echo.
echo ==^> Step 2/2: verifying output ...
if exist "dist-portable\VRchat-LocalCinema-v*-base.zip" (
    echo [OK] dist-portable\VRchat-LocalCinema-v*-base.zip
) else (
    echo [WARN] base zip not found
)
if exist "dist-portable\VRchat-LocalCinema-v*-full" (
    echo [OK] dist-portable\VRchat-LocalCinema-v*-full\
) else (
    echo [WARN] full folder not found - put ffmpeg/easytier binaries into components\ first
)

echo.
echo [DONE] All output in dist-portable\
pause
exit /b 0
