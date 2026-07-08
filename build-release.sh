#!/usr/bin/env sh
set -eu

cargo fmt --check
cargo check
cargo build --release

printf 'Release binary: %s\n' "$(pwd)/target/release/mail-box"
