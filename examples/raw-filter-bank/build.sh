#!/usr/bin/env sh
# Build the filter bank library into this package for the running architecture.
set -eu
cd "$(dirname "$0")"
mkdir -p "lib/$(uname -m)"
cc -shared -fPIC -O2 -Wall -Wextra -o "lib/$(uname -m)/liblowpass.so" lowpass.c
