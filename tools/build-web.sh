#!/usr/bin/env bash
# Build the browser artifacts and report what each costs to download.
#
# There are two on purpose. The plain demo and the formula page are separate
# modules rather than one build with a runtime switch: Typst is the expensive
# dependency by an order of magnitude, and a reader of the Pythagorean scene
# should not download a typesetting engine to see four squares. The sizes are
# printed rather than checked — they are the number §10.6 asks for ("Typst's
# WASM size needs measuring"), and a number in a comment goes stale silently
# while a printed one is in every transcript.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

target=wasm32-unknown-unknown

# `stat` is not portable between GNU and BSD: coreutils spells it `-c%s`, macOS and
# the BSDs spell it `-f%z`. This script runs from a checkout on all three desktop
# platforms, so the spelling is selected rather than assumed — `-c%s` under
# `set -euo pipefail` does not merely print a wrong number, it aborts the build.
case "$(uname -s)" in
  Darwin | *BSD) file_size() { stat -f%z "$1"; } ;;
  *) file_size() { stat -c%s "$1"; } ;;
esac

build() {
  local out=$1 profile=$2
  shift 2
  cargo build --target "$target" --profile "$profile" -p rustim-web "$@"
  # `--target web` is an ES module whose default export is the init function —
  # what `demo.js` imports. The glue and the crate must be the same wasm-bindgen
  # release (`Cargo.toml` pins it), or the module refuses to load.
  wasm-bindgen --target web --no-typescript --out-dir "$out" \
    "target/$target/$profile/rustim_web.wasm"

  local wasm=$out/rustim_web_bg.wasm
  local raw packed glue
  raw=$(file_size "$wasm")
  packed=$(gzip -9 -c "$wasm" | wc -c)
  glue=$(file_size "$out/rustim_web.js")
  printf '%-12s wasm %8s bytes raw  %8s bytes gzip -9   js %6s bytes\n' \
    "$out" "$raw" "$packed" "$glue"
}

build web/pkg web
build web/pkg-formula web-formula --features formula
