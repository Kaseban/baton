#!/usr/bin/env bash
# Fail if version drifts between Cargo.toml, npm/package.json,
# npm/npm-shrinkwrap.json, server.json, and the npm binary download URL.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo_v=$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)"/\1/')
npm_v=$(node -p "require('./npm/package.json').version")
shrink_v=$(node -p "require('./npm/npm-shrinkwrap.json').version")
server_v=$(node -p "require('./server.json').version")
server_pkg_v=$(node -p "require('./server.json').packages[0].version")
url_v=$(node -p "require('./npm/package.json').artifactDownloadUrls[0].match(/download\/v([0-9.]+)/)[1]")

echo "Cargo.toml:            $cargo_v"
echo "npm/package.json:      $npm_v"
echo "npm-shrinkwrap.json:   $shrink_v"
echo "server.json:           $server_v"
echo "server.json package:   $server_pkg_v"
echo "binary download URL:   v$url_v"

if [ "$cargo_v" = "$npm_v" ] && [ "$npm_v" = "$shrink_v" ] && \
   [ "$shrink_v" = "$server_v" ] && [ "$server_v" = "$server_pkg_v" ] && \
   [ "$server_pkg_v" = "$url_v" ]; then
  echo "OK: all versions in sync ($cargo_v)"
else
  echo "ERROR: version drift detected" >&2
  exit 1
fi
