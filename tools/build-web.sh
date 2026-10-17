#!/usr/bin/env bash
# Build the browser artifact and report what it costs to download.
#
# The sizes are printed rather than checked: they are the number §10.6 asks for
# ("Typst's WASM size needs measuring"), and a number in a comment goes stale
# silently while a printed one is in every transcript.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

target=wasm32-unknown-unknown
out=web/pkg

mkdir -p "$out"
cargo build --target "$target" --release -p rustim-web
# `--target web` is an ES module whose default export is the init function —
# what `demo.js` imports. The glue and the crate must be the same wasm-bindgen
# release (`Cargo.toml` pins it), or the module refuses to load.
wasm-bindgen --target web --no-typescript --out-dir "$out" \
  "target/$target/release/rustim_web.wasm"

wasm=$out/rustim_web_bg.wasm
raw=$(stat -c%s "$wasm")
packed=$(gzip -9 -c "$wasm" | wc -c)
glue=$(stat -c%s "$out/rustim_web.js")
printf 'wasm  %8s bytes raw  %8s bytes gzip -9\n' "$raw" "$packed"
printf 'js    %8s bytes\n' "$glue"
