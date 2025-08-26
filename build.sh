#!/bin/bash

cargo skyline build --release --features verbose_logging
cp ./target/aarch64-skyline-switch/release/libstratus_mod_loader.nro ~/.local/share/yuzu/sdmc/atmosphere/contents/01006A800016E000/romfs/skyline/plugins/libstratus.nro
