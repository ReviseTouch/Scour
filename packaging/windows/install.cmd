@echo off
rem Install Scour for this account: no administrator, nothing outside your profile.
rem Files to %LOCALAPPDATA%\Programs\Scour, a Start menu entry, and the folder on PATH.
"%~dp0scour-gui.exe" --install
