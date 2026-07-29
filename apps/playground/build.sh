#!/bin/sh
# Build the playground's wasm module and place it beside index.html for local
# development. Deployment (issue #58) fetches a published artifact instead.
set -eu
cd "$(dirname "$0")/../.."
cargo build -p steins-wasm --target wasm32-unknown-unknown --release
cp target/wasm32-unknown-unknown/release/steins_wasm.wasm apps/playground/steins_wasm.wasm
ls -la apps/playground/steins_wasm.wasm
