#!/bin/bash
cd "$(dirname "$0")"
set -euxo pipefail

cargo build
(
    cd vscode/
    npm i
    npx @vscode/vsce package --allow-missing-repository --skip-license
)
