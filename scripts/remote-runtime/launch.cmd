@echo off
rem Windows counterpart of ./launch. Reconnects run it as cmd.exe /d /c "...\launch.cmd" agent --stdio.
setlocal
for %%I in ("%~dp0.") do set "AGENTWORKS_RUNTIME_DIR=%%~fI"
rem mise is optional: when present, agent CLIs it manages are on the worker's PATH.
where mise >nul 2>nul
if not errorlevel 1 (
  mise -C "%AGENTWORKS_RUNTIME_DIR%" exec -- "%AGENTWORKS_RUNTIME_DIR%\agentworks.exe" %*
  exit /b
)
"%AGENTWORKS_RUNTIME_DIR%\agentworks.exe" %*
