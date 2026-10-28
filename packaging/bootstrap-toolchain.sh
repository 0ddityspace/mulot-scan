#!/bin/sh
# Installe dans <dossier> le compilateur Rust officiel de même version que le
# `rustc` du système, avec la bibliothèque standard des cibles demandées.
# Utile quand Rust vient de la distribution (sans rustup) : les bibliothèques
# standard officielles ne sont pas compatibles avec un rustc recompilé.
#
# Utilisation : bootstrap-toolchain.sh <dossier> <cible>...
set -eu

dest=$1
shift
version=$(rustc --version | awk '{print $2}')
host=$(rustc -vV | sed -n 's/^host: //p')

ready=1
if [ -x "$dest/bin/rustc" ] && [ "$("$dest/bin/rustc" --version | awk '{print $2}')" = "$version" ]; then
    for target in "$host" "$@"; do
        [ -d "$dest/lib/rustlib/$target/lib" ] || ready=0
    done
else
    ready=0
fi
if [ "$ready" = 1 ]; then
    exit 0
fi

echo "Téléchargement de Rust $version officiel ($host + $*) dans $dest"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$dest"
for component in "rustc-$version-$host" "rust-std-$version-$host"; do
    curl -sSfL "https://static.rust-lang.org/dist/$component.tar.xz" | tar -xJ -C "$tmp"
    "$tmp/$component/install.sh" --prefix="$dest" --disable-ldconfig >/dev/null
done
for target in "$@"; do
    component="rust-std-$version-$target"
    curl -sSfL "https://static.rust-lang.org/dist/$component.tar.xz" | tar -xJ -C "$tmp"
    "$tmp/$component/install.sh" --prefix="$dest" --disable-ldconfig >/dev/null
done
"$dest/bin/rustc" --version
