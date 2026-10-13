#!/bin/bash
# Assemble the Volumio plugin zip: the plugin directory, the daemon into
# bin/<arch>, the tap into alsa-lib/<arch>, and the node modules. Output:
# dist/lcd1602_vumeter-<version>.zip, with the plugin's files at its root
# as Volumio's plugin manager expects.
set -euo pipefail
ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n1)
STAGE=$ROOT/dist/stage/lcd1602_vumeter
rm -rf "$ROOT/dist/stage"
mkdir -p "$STAGE" "$ROOT/dist"
cp -r "$ROOT/plugin/." "$STAGE/"
rm -rf "$STAGE/test" "$STAGE/asound"
mkdir -p "$STAGE/asound"

# The plugin version follows the workspace version.
sed -i "s/\"version\": \"[^\"]*\"/\"version\": \"$VERSION\"/" "$STAGE/package.json"

# The build the zip carries: the commit and the time.
printf '{ "commit": "%s", "built": "%s" }\n' \
  "$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)" \
  "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$STAGE/build.json"

shipped=0
for arch in armhf arm64 amd64; do
  if [ -x "$ROOT/bin/$arch/lcd1602-vumeter" ] && [ -f "$ROOT/lib/$arch/libvutap.so" ]; then
    install -D -m 755 "$ROOT/bin/$arch/lcd1602-vumeter" "$STAGE/bin/$arch/lcd1602-vumeter"
    install -D -m 644 "$ROOT/lib/$arch/libvutap.so" "$STAGE/alsa-lib/$arch/libvutap.so"
    shipped=$((shipped + 1))
  fi
done
if [ "$shipped" = 0 ]; then
  echo "package: no binaries under bin/ and lib/; run scripts/ship.sh first" >&2
  exit 1
fi

# Node modules: with npm on this machine, directly; otherwise in a container.
if command -v npm >/dev/null 2>&1; then
  ( cd "$STAGE" && npm install --omit=dev --no-audit --no-fund --loglevel=error >/dev/null )
else
  docker run --rm -v "$STAGE:/plugin" -w /plugin node:20-slim sh -c "npm install --omit=dev --no-audit --no-fund --loglevel=error >/dev/null && chown -R $(id -u):$(id -g) /plugin/node_modules /plugin/package-lock.json 2>/dev/null || true"
fi
rm -f "$STAGE/package-lock.json"

( cd "$STAGE" && rm -f "$ROOT/dist/lcd1602_vumeter-$VERSION.zip" && zip -qr "$ROOT/dist/lcd1602_vumeter-$VERSION.zip" . )
rm -rf "$ROOT/dist/stage"
ls -la "$ROOT/dist/lcd1602_vumeter-$VERSION.zip"
