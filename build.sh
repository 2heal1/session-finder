#!/bin/sh
set -eu

project_dir=$(dirname "$0")
build_dir=${CARGO_TARGET_DIR:-"$project_dir/target"}

# Use the installed standalone toolchain without changing xcode-select globally.
if [ "$(uname -s)" = Darwin ] && [ -x /Library/Developer/CommandLineTools/usr/bin/clang ]; then
  export DEVELOPER_DIR=${DEVELOPER_DIR:-/Library/Developer/CommandLineTools}
  export SDKROOT=${SDKROOT:-"$DEVELOPER_DIR/SDKs/MacOSX.sdk"}
  export CC=${CC:-"$DEVELOPER_DIR/usr/bin/clang"}
  export CXX=${CXX:-"$DEVELOPER_DIR/usr/bin/clang++"}
  case "$(uname -m)" in
    arm64) export CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=${CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER:-"$CC"} ;;
    x86_64) export CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER=${CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER:-"$CC"} ;;
  esac
fi

if [ "${1:-}" = check ]; then
  cargo fmt --manifest-path "$project_dir/Cargo.toml" --check
  cargo clippy --locked --manifest-path "$project_dir/Cargo.toml" --target-dir "$build_dir" --all-targets -- -D warnings
  cargo test --locked --manifest-path "$project_dir/Cargo.toml" --target-dir "$build_dir"
else
  cargo build --locked --release --manifest-path "$project_dir/Cargo.toml" --target-dir "$build_dir"
  cp "$build_dir/release/session-finder" "$project_dir/session-finder"
  printf 'Built: %s/session-finder\n' "$project_dir"
fi
