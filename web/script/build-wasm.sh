#!/bin/bash
set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${GREEN}Building Coop Web...${NC}"

# Get the script directory
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
PROJECT_ROOT="$SCRIPT_DIR/../.."
WEB_ROOT="$SCRIPT_DIR/.."

# Parse arguments
RELEASE_FLAG=""
if [[ "$1" == "--release" ]]; then
    RELEASE_FLAG="--release"
    echo -e "${YELLOW}Building in release mode${NC}"
fi

# wasm_thread requires a nightly compiler for its atomics features
CARGO="cargo +nightly"

# Step 0: Toolchain prerequisites
echo -e "${GREEN}Step 0: Checking toolchain...${NC}"
rustup target add wasm32-unknown-unknown --toolchain nightly

if [[ "$(uname)" == "Darwin" ]]; then
    # secp256k1-sys compiles C for wasm; Apple clang has the wasm32 target and
    # llvm-ar for archives comes from the rustup llvm-tools component
    if ! xcrun --find clang &>/dev/null; then
        echo -e "${RED}Apple clang not found. Install the Xcode command line tools: xcode-select --install${NC}"
        exit 1
    fi
    rustup component add llvm-tools-preview --toolchain nightly

    RUSTUP_HOME=$(rustup show home)
    if [[ "$(uname -m)" == "arm64" ]]; then
        HOST_ARCH="aarch64-apple-darwin"
    else
        HOST_ARCH="x86_64-apple-darwin"
    fi
    LLVM_BIN="$RUSTUP_HOME/toolchains/nightly-$HOST_ARCH/lib/rustlib/$HOST_ARCH/bin"

    export CC_wasm32_unknown_unknown=clang
    export AR_wasm32_unknown_unknown="$LLVM_BIN/llvm-ar"
    echo -e "${GREEN}Using Apple clang + rustup llvm-ar for wasm32 C code${NC}"
fi

# Match wasm-bindgen-cli to the version in Cargo.lock
LOCKED_BINDGEN=$(grep -A1 '^name = "wasm-bindgen"$' "$PROJECT_ROOT/Cargo.lock" | grep version | cut -d'"' -f2)
if [[ "$(wasm-bindgen --version 2>/dev/null | awk '{print $2}')" != "$LOCKED_BINDGEN" ]]; then
    echo -e "${YELLOW}Installing wasm-bindgen-cli $LOCKED_BINDGEN to match Cargo.lock...${NC}"
    cargo install -f wasm-bindgen-cli --version "$LOCKED_BINDGEN"
fi

# Step 1: Build WASM
echo -e "${GREEN}Step 1: Building WASM...${NC}"
export CARGO_TARGET_DIR="$PROJECT_ROOT/target"
RUSTFLAGS='-C target-feature=+bulk-memory -C link-arg=--max-memory=4294967296' \
    $CARGO build -p coop_web --target wasm32-unknown-unknown $RELEASE_FLAG

# Determine the build directory
if [[ "$RELEASE_FLAG" == "--release" ]]; then
    BUILD_MODE="release"
else
    BUILD_MODE="debug"
fi

# WASM file is in workspace target directory
WASM_PATH="$PROJECT_ROOT/target/wasm32-unknown-unknown/$BUILD_MODE/coop_web.wasm"

# Check if WASM file exists
if [[ ! -f "$WASM_PATH" ]]; then
    echo -e "${RED}Error: WASM file not found at: $WASM_PATH${NC}"
    exit 1
fi

# Step 2: Generate JavaScript bindings
echo -e "${GREEN}Step 2: Generating JavaScript bindings...${NC}"
wasm-bindgen "$WASM_PATH" \
    --out-dir "$WEB_ROOT/www/wasm" \
    --target web \
    --no-typescript

echo -e "${GREEN}✓ Build completed successfully!${NC}"
echo -e "${YELLOW}Next steps:${NC}"
echo -e "  cd web/www"
echo -e "  npm install"
echo -e "  npm run dev"
