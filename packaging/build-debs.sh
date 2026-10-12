#!/usr/bin/env bash
# Builds manageriliki-tui and manageriliki-gui .deb packages into dist/.
set -euo pipefail
cd "$(dirname "$0")/.."

image=manageriliki-deb-builder
docker build -t "$image" -f packaging/Dockerfile.deb packaging

mkdir -p dist
# A separate target dir keeps these glibc-2.35 builds apart from host builds.
docker run --rm \
    --user "$(id -u):$(id -g)" \
    -v "$PWD":/src -w /src \
    -e CARGO_TARGET_DIR=/src/target/deb \
    -e HOME=/tmp \
    "$image" \
    bash -c 'for p in manageriliki-tui manageriliki-gui; do cargo deb --locked -p "$p" -o dist/; done'

ls -l dist/*.deb
