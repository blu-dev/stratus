#!/bin/bash
cargo skyline build --release --features sanity_checks
cp ./target/aarch64-skyline-switch/release/libstratus_mod_loader.nro ~/.config/Ryujinx/sdcard/atmosphere/contents/01006A800016E000/romfs/skyline/plugins/libstratus.nro
cp ./target/aarch64-skyline-switch/release/libstratus_mod_loader.nro ~/.local/share/yuzu/sdmc/atmosphere/contents/01006A800016E000/romfs/skyline/plugins/libstratus.nro

if [ $1 == "switch" ]; then
    curl -T ./target/aarch64-skyline-switch/release/libstratus_mod_loader.nro ftp://192.168.0.8:5000/atmosphere/contents/01006A800016E000/romfs/skyline/plugins/libstratus.nro
fi
