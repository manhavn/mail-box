#!/usr/bin/env sh
set -eu

APP_NAME="mail-box"
BUILD_ALL=0
RUN_CHECKS=1
TARGET=""

TARGETS="\
x86_64-unknown-linux-gnu\
aarch64-unknown-linux-gnu\
x86_64-unknown-linux-musl\
aarch64-unknown-linux-musl\
x86_64-pc-windows-gnu\
aarch64-pc-windows-gnullvm\
x86_64-apple-darwin\
aarch64-apple-darwin\
"

usage() {
  cat <<'EOF'
Usage:
  ./build-release.sh [options]

Options:
  --native              Build the current machine target. This is the default.
  --target <triple>     Build one Rust target triple.
  --all                 Build common Linux, Windows, and macOS amd64/arm64 targets.
  --no-checks           Skip cargo fmt --check and cargo check.
  -h, --help            Show this help.

Common targets:
  x86_64-unknown-linux-gnu       Linux amd64 glibc
  aarch64-unknown-linux-gnu      Linux arm64 glibc
  x86_64-unknown-linux-musl      Linux amd64 static/musl
  aarch64-unknown-linux-musl     Linux arm64 static/musl
  x86_64-pc-windows-gnu          Windows amd64
  aarch64-pc-windows-gnullvm     Windows arm64
  x86_64-apple-darwin            macOS amd64
  aarch64-apple-darwin           macOS arm64

Notes:
  Native builds only need cargo.
  Cross builds work best with cargo-zigbuild and zig installed:
    cargo install cargo-zigbuild
    https://ziglang.org/download/
  macOS targets still require Apple's SDK/toolchain and are normally built on macOS.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --native)
      TARGET=""
      BUILD_ALL=0
      ;;
    --target)
      if [ "$#" -lt 2 ]; then
        printf 'Error: --target requires a target triple.\n' >&2
        exit 1
      fi
      TARGET="$2"
      BUILD_ALL=0
      shift
      ;;
    --all)
      BUILD_ALL=1
      TARGET=""
      ;;
    --no-checks)
      RUN_CHECKS=0
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'Error: unknown option: %s\n\n' "$1" >&2
      usage >&2
      exit 1
      ;;
  esac
  shift
done

if [ "$RUN_CHECKS" -eq 1 ]; then
  cargo fmt --check
  cargo check
fi

build_native() {
  cargo build --release
  printf 'Release binary: %s\n' "$(pwd)/target/release/$APP_NAME"
}

build_target() {
  target="$1"
  binary_name="$APP_NAME"

  case "$target" in
    *windows*) binary_name="$APP_NAME.exe" ;;
  esac

  if command -v cargo-zigbuild >/dev/null 2>&1; then
    cargo zigbuild --release --target "$target"
  else
    rustup target add "$target"
    cargo build --release --target "$target"
  fi

  printf 'Release binary: %s\n' "$(pwd)/target/$target/release/$binary_name"
}

if [ "$BUILD_ALL" -eq 1 ]; then
  for target in $TARGETS; do
    printf '\n==> Building %s\n' "$target"
    build_target "$target"
  done
elif [ -n "$TARGET" ]; then
  build_target "$TARGET"
else
  build_native
fi
