#!/bin/bash
# The workshop pass CI runs: formatting, lints with warnings denied, tests,
# the documentation with warnings denied, the plugin's files and scripts.
# Run it before a commit.
set -euo pipefail
ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
cd "$ROOT"

echo "check: format"
cargo fmt --all -- --check

echo "check: clippy"
cargo clippy --workspace --all-targets --locked -- -D warnings

echo "check: tests"
cargo test --workspace --locked

echo "check: documentation"
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked

echo "check: glibc"
# The daemon must load on Volumio's glibc 2.36: a symbol versioned above it,
# even a weak one, makes the loader refuse the whole binary. The release
# build refuses such a binary (scripts/ship.sh); here the same is asked of
# this machine's build, which shows it wherever this machine's glibc is
# newer than the player's.
cargo build --locked -p lcd1602-vumeter
newest=$(readelf -W --dyn-syms target/debug/lcd1602-vumeter | grep -o 'GLIBC_2\.[0-9]*' | sort -t. -k2,2n -u | tail -n1)
case "$newest" in
  GLIBC_2.3[7-9]|GLIBC_2.[4-9]*|GLIBC_2.[1-9][0-9][0-9])
    echo "check: the daemon refers to $newest, newer than Volumio's glibc 2.36:" >&2
    readelf -W --dyn-syms target/debug/lcd1602-vumeter | grep "$newest" >&2
    exit 1
    ;;
esac

echo "check: renders"
# Every screen the daemon draws, rendered from the simulated display.
cargo run -q --locked -p lcd1602-vumeter -- --render target/renders >/dev/null

echo "check: shell scripts"
for script in scripts/*.sh plugin/*.sh plugin/scripts/*.sh; do
  bash -n "$script" || { echo "check: $script does not parse" >&2; exit 1; }
done
if command -v shellcheck >/dev/null 2>&1; then
  # SC1007 is the `CDPATH= cd` idiom; SC2012 an `ls` in a message.
  shellcheck -x --source-path=SCRIPTDIR -e SC1007,SC2012 scripts/*.sh plugin/*.sh plugin/scripts/*.sh
fi

# Node runs the checks of the plugin's files: here where it is installed,
# else in the node image.
if command -v node >/dev/null 2>&1; then
  with_node() { node "$@"; }
elif command -v docker >/dev/null 2>&1; then
  with_node() { docker run -i --rm -v "$ROOT:/work" -w /work node:20-slim node "$@"; }
else
  echo "check: neither node nor docker found; the plugin's checks are skipped" >&2
  exit 0
fi

echo "check: plugin files"
# Volumio's core reads every plugin's strings at its start: one bad file
# takes the whole backend down, so every JSON the plugin ships must parse,
# and a key written twice in one object (the later silently wins) is refused.
with_node - plugin/package.json plugin/config.json plugin/UIConfig.json plugin/i18n/*.json <<'JS'
const fs = require('fs');
function keysWrittenTwice(text) {
  const twice = new Set();
  const stack = [];
  let i = 0;
  let start = 0;
  const skipString = () => { i++; while (i < text.length && text[i] !== '"') { if (text[i] === '\\') i++; i++; } return text.slice(start + 1, i++); };
  while (i < text.length) {
    const c = text[i];
    if (c === '{') { stack.push({ keys: new Set(), expectKey: true }); i++; }
    else if (c === '}') { stack.pop(); i++; }
    else if (c === '[') { stack.push(null); i++; }
    else if (c === ']') { stack.pop(); i++; }
    else if (c === '"') {
      start = i;
      const s = skipString();
      const top = stack[stack.length - 1];
      if (top && top.expectKey) { if (top.keys.has(s)) twice.add(s); top.keys.add(s); top.expectKey = false; }
    }
    else if (c === ',') { const top = stack[stack.length - 1]; if (top) top.expectKey = true; i++; }
    else i++;
  }
  return [...twice].sort();
}
const files = process.argv.slice(2);
for (const file of files) {
  const text = fs.readFileSync(file, 'utf8');
  try { JSON.parse(text); } catch (e) { console.error(file + ': ' + e.message); process.exit(1); }
  const twice = keysWrittenTwice(text);
  if (twice.length) { console.error(file + ': written twice in one object: ' + twice.join(', ')); process.exit(1); }
  if (!text.endsWith('\n') || text.replace(/\n+$/, '') !== text.trimEnd()) { console.error(file + ': must end with one newline'); process.exit(1); }
}
console.log(files.length + ' plugin files parse');
JS

echo "check: UI strings"
# Every TRANSLATE key the settings page names must be in strings_en.json.
with_node - plugin/UIConfig.json plugin/i18n/strings_en.json <<'JS'
const fs = require('fs');
const ui = fs.readFileSync(process.argv[2], 'utf8');
const strings = JSON.parse(fs.readFileSync(process.argv[3], 'utf8'));
const missing = [...new Set([...ui.matchAll(/TRANSLATE\.([A-Z0-9_.]+)/g)].map((m) => m[1]))]
  .filter((key) => key.split('.').reduce((o, k) => (o && typeof o === 'object' ? o[k] : undefined), strings) === undefined);
if (missing.length) { console.error('strings missing: ' + missing.join(', ')); process.exit(1); }
console.log('every UI string is defined');
JS

echo "check: plugin"
with_node --check plugin/index.js plugin/lib/boot.js
with_node --test plugin/test/

echo "check: clean"
