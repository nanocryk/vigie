@echo off
rem Runs serve.sh with the Git for Windows bash. The bash on PATH may be WSL's.
setlocal
cd /d "%~dp0"
set "GIT_BASH="
for /f "delims=" %%G in ('where git 2^>nul') do (
  if not defined GIT_BASH if exist "%%~dpG..\bin\bash.exe" set "GIT_BASH=%%~dpG..\bin\bash.exe"
  if not defined GIT_BASH if exist "%%~dpG..\..\bin\bash.exe" set "GIT_BASH=%%~dpG..\..\bin\bash.exe"
)
if not defined GIT_BASH if exist "%ProgramFiles%\Git\bin\bash.exe" set "GIT_BASH=%ProgramFiles%\Git\bin\bash.exe"
if not defined GIT_BASH (
  echo Git Bash not found: install Git for Windows.
  exit /b 1
)
"%GIT_BASH%" serve.sh %*
exit /b %ERRORLEVEL%
