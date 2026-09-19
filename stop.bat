@echo off
rem Stops the Companion Lobby connector (the companion manager, or a serve process).
rem The tray's Quit is the clean path; this script asks the process to close and,
rem if it cannot, stops it firmly and clears the lock a firm stop leaves behind.
setlocal
set "HOME_DIR=%USERPROFILE%\.companion-lobby"
if defined COMPANION_LOBBY_HOME set "HOME_DIR=%COMPANION_LOBBY_HOME%"

tasklist /FI "IMAGENAME eq companion-lobby.exe" | findstr /I "companion-lobby.exe" >nul
if not %errorlevel%==0 (
    echo The connector is not running.
    if exist "%HOME_DIR%\lock" (
        echo Cleared a stale data-directory lock left by an unclean stop.
        del "%HOME_DIR%\lock"
    )
    exit /b 0
)

rem Ask nicely first: a close lets the process clear its recorded state.
taskkill /IM companion-lobby.exe >nul 2>&1
ping -n 3 127.0.0.1 >nul

tasklist /FI "IMAGENAME eq companion-lobby.exe" | findstr /I "companion-lobby.exe" >nul
if %errorlevel%==0 (
    taskkill /F /IM companion-lobby.exe >nul 2>&1
    ping -n 2 127.0.0.1 >nul
)

tasklist /FI "IMAGENAME eq companion-lobby.exe" | findstr /I "companion-lobby.exe" >nul
if not %errorlevel%==0 (
    echo The connector has stopped.
    if exist "%HOME_DIR%\lock" del "%HOME_DIR%\lock"
    exit /b 0
)
echo The connector could not be stopped; try the tray's Quit, or run this as Administrator.
exit /b 1
