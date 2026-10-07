#!/usr/bin/env bash
# Build the Modelica Association Reference FMUs (BSD-2) for this host into "$1".
# Layout: $1/fmi{2,3}/<Model>/{modelDescription.xml,binaries/<platform>/<Model>.so}.
# Compiled with cc directly: upstream CMake has no FMI 2 platform for aarch64.
set -euo pipefail

out=$1
# Upstream main after v0.0.41: carries the StateSpace setUInt64 fix (#692).
rev=86ca571ec1ed4bbb4950a9355c2b88ff8ec2f78e
src=$out/src-$rev
[ -f "$out/.done-$rev" ] && [ -e "$out/src" ] && exit 0

if [ ! -d "$src" ]; then
    rm -rf "$src.tmp"
    git init --quiet "$src.tmp"
    git -C "$src.tmp" fetch --quiet --depth 1 https://github.com/modelica/Reference-FMUs "$rev"
    git -C "$src.tmp" checkout --quiet FETCH_HEAD
    mv "$src.tmp" "$src"
fi

case "$(uname -m)" in
    x86_64) p3=x86_64-linux; p2=linux64 ;;
    aarch64) p3=aarch64-linux; p2=aarch64-linux ;;
    *) echo "unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac

build() { # version platform model
    local v=$1 plat=$2 m=$3 dir=$out/fmi$1/$3
    mkdir -p "$dir/binaries/$plat"
    cc -shared -fPIC -O2 -fvisibility=hidden -DFMI_VERSION="$v" -DDISABLE_PREFIX \
        -I"$src/include" -I"$src/$m" \
        "$src/$m/model.c" "$src/src/fmi${v}Functions.c" "$src/src/cosimulation.c" \
        -o "$dir/binaries/$plat/$m.so"
    cp "$src/$m/FMI$v.xml" "$dir/modelDescription.xml"
}

for m in BouncingBall Dahlquist VanDerPol Feedthrough; do
    build 2 "$p2" "$m"
    build 3 "$p3" "$m"
done
build 3 "$p3" StateSpace

ln -sfn "src-$rev" "$out/src"
touch "$out/.done-$rev"
