#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
if ! command -v cargo >/dev/null 2>&1; then
    printf '%s\n' 'Rust is required to build Monet. On Ubuntu: sudo apt install rustc cargo' >&2
    exit 1
fi
cargo build --release --offline --locked
cp target/release/monet .monet-build
chmod +x .monet-build
mv .monet-build monet
printf '\n%s\n' '🌐 Built ./monet' 'Run ./monet to open the terminal dashboard.'
