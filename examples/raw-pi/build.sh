#!/usr/bin/env sh
# Build the PI library into this package for the running architecture.
set -eu
cd "$(dirname "$0")"
mkdir -p "lib/$(uname -m)"
cc -shared -fPIC -O2 -Wall -o "lib/$(uname -m)/libpi.so" pi.c
