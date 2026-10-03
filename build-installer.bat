@echo off
rem Builds the MSI installer. Same as: powershell -ExecutionPolicy Bypass -File build-installer.ps1
setlocal
cd /d "%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0build-installer.ps1" %*
if errorlevel 1 (
  echo.
  echo The installer build did not finish. See the messages above.
  pause
  exit /b 1
)
echo.
echo Installer is in the dist folder.
pause
endlocal
