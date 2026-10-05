@echo off
rem Take Scour away again: the Start menu entry, the PATH entry, starting with
rem Windows, and the program folder. The index and settings stay in
rem %LOCALAPPDATA%\scour and %APPDATA%\scour; delete those too for a clean slate.
"%~dp0scour-gui.exe" --uninstall
taskkill /f /im scourd.exe >nul 2>&1
taskkill /f /im scour-web.exe >nul 2>&1
cd /d "%TEMP%"
rmdir /s /q "%LOCALAPPDATA%\Programs\Scour"
rem Once more after a moment: a folder still in use is not removed the first time.
if exist "%LOCALAPPDATA%\Programs\Scour" ping -n 2 127.0.0.1 >nul & rmdir /s /q "%LOCALAPPDATA%\Programs\Scour"
