@echo off
rem [UNTITLED] TOOLS launcher. Runs the app; builds it first if needed.
rem Extra arguments are passed through, e.g.  launch.bat --serve
setlocal
cd /d "%~dp0"

if exist "dist\untitled-tools.exe" goto run

echo First run: building [UNTITLED] TOOLS ...
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0build.ps1" -NoRun
if exist "dist\untitled-tools.exe" goto run

echo.
echo The PowerShell builder did not finish. Trying cargo directly ...
where cargo >nul 2>nul
if errorlevel 1 (
  if exist "%USERPROFILE%\.cargo\bin\cargo.exe" set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
)
where cargo >nul 2>nul
if errorlevel 1 goto nocargo
cargo build --release
if errorlevel 1 goto failed
if not exist dist mkdir dist
copy /y "target\release\untitled-tools.exe" "dist\untitled-tools.exe" >nul
goto run

:nocargo
echo Rust (cargo) is not installed. Install it from https://rustup.rs and the
echo Microsoft C++ Build Tools, then run this file again.
goto failed

:failed
echo.
echo Build failed. See the messages above.
pause
exit /b 1

:run
"dist\untitled-tools.exe" %*
if errorlevel 1 pause
endlocal
