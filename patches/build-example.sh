#!/bin/bash
export RUSTUP_HOME='D:\rust\rustup' CARGO_HOME='D:\rust\cargo'
export PATH="/d/rust/cargo/bin:$PATH"
export CARGO_NET_GIT_FETCH_WITH_CLI=true
export GPUI_FXC_PATH='C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\fxc.exe'
export CARGO_PROFILE_RELEASE_DEBUG=0
export CARGO_TARGET_DIR='D:\jcode-build\jcode-desktop\target'
cd /d/jcode-build/jcode-desktop-features
cargo build --release --bin jcode-desktop -j 2 2>&1 | grep -v "^\s*Compiling\|^warning: unused" | tail -120
echo "EXIT ${PIPESTATUS[0]}"
