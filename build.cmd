@echo off
call "D:\Visual Studio\VC\Auxiliary\Build\vcvars64.bat" >nul
set RUSTUP_HOME=D:\rust\.rustup
set CARGO_HOME=D:\rust\.cargo
set PATH=D:\rust\.cargo\bin;%PATH%
set CARGO_BUILD_JOBS=6
set GPUI_FXC_PATH=D:\Windows Kits\10\bin\10.0.26100.0\x64\fxc.exe
cargo +stable %*
