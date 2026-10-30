@echo off
rem Windows starts no scripts by their #! line: the fake CLI through Python, in UTF-8.
set PYTHONUTF8=1
python "%~dp0fake-claude.py" %*
