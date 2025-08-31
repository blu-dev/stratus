#!/bin/bash
cargo skyline build --release

if [ $1 == "ryu" ]; then
    cargo skyline build --release && \
        cp ./target/aarch64-skyline-switch/release/libstratus_mod_loader.nro ~/.config/Ryujinx/sdcard/atmosphere/contents/01006A800016E000/romfs/skyline/plugins/libstratus.nro && \
        ~/Downloads/ryujinx-test/ryujinx.AppImage ~/Documents/yuzu_games/Super\ Smash\ Bros.\ Ultimate.xci
fi
if [ $1 == "yuzu" ]; then
    cargo skyline build --release && \
        cp ./target/aarch64-skyline-switch/release/libstratus_mod_loader.nro ~/.local/share/yuzu/sdmc/atmosphere/contents/01006A800016E000/romfs/skyline/plugins/libstratus.nro && \
        ./build.sh && ~/Downloads/yuzu-mainline-20240304-537296095.AppImage ~/Documents/yuzu_games/Super\ Smash\ Bros.\ Ultimate.xci 2>&1
fi
if [ $1 == "switch" ]; then
    cargo skyline build --release && \
        curl -T ./target/aarch64-skyline-switch/release/libstratus_mod_loader.nro ftp://192.168.0.8:5000/atmosphere/contents/01006A800016E000/romfs/skyline/plugins/libstratus.nro
fi
