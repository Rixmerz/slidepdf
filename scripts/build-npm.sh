#!/usr/bin/env sh
# Builds the npm package in npm/: WebAssembly for Node and for the web,
# plus the viewer. Needs the wasm32-unknown-unknown target and
# wasm-bindgen-cli matching the wasm-bindgen version in bindings/wasm.
set -eu
cd "$(dirname "$0")/.."
TARGET_DIR=${CARGO_TARGET_DIR:-target}
WASM=$TARGET_DIR/wasm32-unknown-unknown/wasm/slidepdf_wasm.wasm
rm -rf npm/dist
build() { # <out-name> <opt-level> <cargo feature args...>
  name=$1; level=$2; shift 2
  CARGO_PROFILE_WASM_OPT_LEVEL=$level cargo build -p slidepdf-wasm --profile wasm --target wasm32-unknown-unknown "$@"
  for target in nodejs web; do
    dir=npm/dist/$name-$([ $target = nodejs ] && echo node || echo web)
    wasm-bindgen "$WASM" --remove-name-section --remove-producers-section --target $target --out-dir "$dir"
    if command -v wasm-opt >/dev/null 2>&1; then
      wasm-opt -Os --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext --enable-mutable-globals \
        "$dir/slidepdf_wasm_bg.wasm" -o "$dir/slidepdf_wasm_bg.wasm"
    fi
  done
}
# Measured: "s" is smaller gzipped for the full build, "z" for lite.
build full s
build lite z --no-default-features
# Node and web builds share one .wasm (they are byte-identical): point the
# Node glue at the web folder and drop its copy.
for v in full lite; do
  cmp -s "npm/dist/$v-node/slidepdf_wasm_bg.wasm" "npm/dist/$v-web/slidepdf_wasm_bg.wasm"
  rm "npm/dist/$v-node/slidepdf_wasm_bg.wasm"
  sed -i.bak "s#\${__dirname}/slidepdf_wasm_bg.wasm#\${__dirname}/../$v-web/slidepdf_wasm_bg.wasm#" "npm/dist/$v-node/slidepdf_wasm.js"
  rm -f "npm/dist/$v-node/slidepdf_wasm.js.bak"
  grep -q "$v-web/slidepdf_wasm_bg.wasm" "npm/dist/$v-node/slidepdf_wasm.js"
done
# The nodejs target emits CommonJS; mark that folder so Node does not treat it as ESM.
for d in npm/dist/*-node; do echo '{ "type": "commonjs" }' > "$d/package.json"; done
rm -f npm/dist/*/.gitignore
mkdir -p npm/viewer
cp viewer/slide-viewer.js npm/viewer/
ls -l npm/dist/*/slidepdf_wasm_bg.wasm
