#!/bin/bash
cd "$(dirname "$0")"
set -euxo pipefail

cargo build --release
mkdir -p vscode/lsp_bin/
cp target/release/prolog-lsp vscode/lsp_bin/
(
    cd vscode/
    npm i
    npx @vscode/vsce package --allow-missing-repository --skip-license --allow-star-activation
)
