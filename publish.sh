#!/usr/bin/env bash
set -euo pipefail

echo "Publishing tweezer..."
cargo publish -p tweezer

VERSION=$(grep '^version' tweezer/Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
echo "Waiting for crates.io to index tweezer $VERSION..."
sleep 10

echo "Publishing tweezer-streamplace..."
cargo publish -p tweezer-streamplace

echo "Done."
