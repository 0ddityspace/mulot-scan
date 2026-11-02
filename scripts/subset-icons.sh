#!/bin/sh
# Rebuilds assets/fonts/icons.ttf: the glyphs of Phosphor.ttf (regular) used
# in src/app/icons.rs. Needs fonttools (pyftsubset).
#   scripts/subset-icons.sh path/to/Phosphor.ttf
set -e
cd "$(dirname "$0")/.."
codes=$(grep -o '\\u{[0-9A-Fa-f]*}' src/app/icons.rs | sed 's/\\u{\(.*\)}/U+\1/' | paste -sd, -)
pyftsubset "$1" --unicodes="$codes" --output-file=assets/fonts/icons.ttf --no-layout-closure --layout-features='' --drop-tables+=GSUB
