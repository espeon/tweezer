#!/usr/bin/env bash
set -euo pipefail

echo "Publishing tweezer..."
cargo publish -p tweezer

VERSION=$(grep '^version' tweezer/Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
echo "Waiting for crates.io to index tweezer $VERSION..."
sleep 10

echo "Updating tweezer-streamplace dep to $VERSION..."
sed -i '' "s/tweezer = { path = \"..\/tweezer\", version = \"[^\"]*\" }/tweezer = { path = \"..\/tweezer\", version = \"$VERSION\" }/" tweezer-streamplace/Cargo.toml

# commit the updated version to git
git add tweezer-streamplace/Cargo.toml
git commit -m "Update tweezer deps to $VERSION"

echo "Publishing tweezer-streamplace..."
cargo publish -p tweezer-streamplace

echo "Done."
