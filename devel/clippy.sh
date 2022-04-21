#!/usr/bin/env bash
set -o errexit
set -o nounset
set -o pipefail

ROOT_DIR=$(realpath $(dirname $(dirname "${BASH_SOURCE}")))

cd "${ROOT_DIR}/pkg/controller" && cargo +nightly clippy --all
cd "${ROOT_DIR}/pkg/kube-rs" && cargo +nightly clippy --all
cd "${ROOT_DIR}/pkg/kube-runtime-abi" && cargo +nightly clippy --all
cd "${ROOT_DIR}/pkg/wasm-delay-queue" && cargo +nightly clippy --all

cd "${ROOT_DIR}/controllers/ring-rust-controller" && cargo +nightly clippy --all
cd "${ROOT_DIR}/controllers/simple-rust-controller" && cargo +nightly clippy --all
