@echo off
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
if errorlevel 1 exit /b 1
cd /d D:\rustterm-tauri\rustterm-egui
set PATH=D:\tools\nasm\nasm-2.16.03;%PATH%
cargo check
