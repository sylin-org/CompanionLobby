@echo off
rem Starts (or restarts) the Companion Lobby connector's companion manager:
rem Existing manager tabs reconnect to the fresh process by themselves.
rem any previous instance is stopped first, then a fresh one launches.
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

rem Any previous instance goes first: one data directory, one process. stop.bat
rem also clears the lock a firm stop leaves behind.
call "%~dp0stop.bat" >nul 2>&1
if errorlevel 1 (
    echo The previous instance could not be stopped; nothing was relaunched.
    exit /b 1
)

rem Nothing runs, so any leftover lock is stale by definition; --force clears it.
set "OUT=%TEMP%\companion-lobby-manager.out.log"
set "ERR=%TEMP%\companion-lobby-manager.err.log"
powershell -NoProfile -Command "Start-Process -FilePath '%BINARY%' -ArgumentList 'manager','--force','--no-open' -WindowStyle Hidden -RedirectStandardOutput '%OUT%' -RedirectStandardError '%ERR%'" >nul 2>&1

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
