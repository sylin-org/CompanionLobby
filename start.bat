@echo off
rem Starts the Companion Lobby connector's companion manager (loopback page + tray).
rem The manager opens your browser by itself once it is up.
setlocal
set "ROOT=%~dp0"
set "BINARY=%ROOT%target\release\companion-lobby.exe"
set "HOME_DIR=%USERPROFILE%\.companion-lobby"
if defined COMPANION_LOBBY_HOME set "HOME_DIR=%COMPANION_LOBBY_HOME%"
set "PORT=5219"
if defined COMPANION_LOBBY_PORT set "PORT=%COMPANION_LOBBY_PORT%"

if not exist "%BINARY%" (
    echo The connector is not built yet. From this repository, run:
    echo     cargo build --release
    exit /b 1
)

tasklist /FI "IMAGENAME eq companion-lobby.exe" | findstr /I "companion-lobby.exe" >nul
if %errorlevel%==0 (
    echo The companion manager is already running at http://127.0.0.1:%PORT%/
    exit /b 0
)

rem With no process running, a leftover lock is stale debris from an unclean stop.
set "STALE="
if exist "%HOME_DIR%\lock" set "STALE=1"
set "OUT=%TEMP%\companion-lobby-manager.out.log"
set "ERR=%TEMP%\companion-lobby-manager.err.log"
if defined STALE (
    echo Cleared a stale data-directory lock left by an unclean stop.
    powershell -NoProfile -Command "Start-Process -FilePath '%BINARY%' -ArgumentList 'manager','--force' -WindowStyle Hidden -RedirectStandardOutput '%OUT%' -RedirectStandardError '%ERR%'" >nul 2>&1
) else (
    powershell -NoProfile -Command "Start-Process -FilePath '%BINARY%' -ArgumentList 'manager' -WindowStyle Hidden -RedirectStandardOutput '%OUT%' -RedirectStandardError '%ERR%'" >nul 2>&1
)

rem Wait (up to ten seconds) for the page to answer.
set /a TRIES=0
:waiting
ping -n 2 127.0.0.1 >nul
curl -s -m 2 -o nul http://127.0.0.1:%PORT%/api/discovery
if %errorlevel%==0 goto ready
tasklist /FI "IMAGENAME eq companion-lobby.exe" | findstr /I "companion-lobby.exe" >nul
if not %errorlevel%==0 goto died
set /a TRIES+=1
if %TRIES% lss 10 goto waiting

echo The manager process is running but the page has not answered yet.
echo If it never comes up, its output is in %ERR%
exit /b 1

:died
echo The manager did not start. Its last words:
type "%ERR%"
exit /b 1

:ready
echo The companion manager is running at http://127.0.0.1:%PORT%/
exit /b 0
