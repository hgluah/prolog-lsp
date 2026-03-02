#!/bin/bash
cd "$(dirname "$0")"
set -euxo pipefail

CARGO_TARGET_DIR=target/ cargo install --path .
(
    cd vscode/
    npm i
    npx @vscode/vsce package --allow-missing-repository --skip-license
)
