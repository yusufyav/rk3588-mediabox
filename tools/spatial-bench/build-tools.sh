#!/usr/bin/env bash
# The appliance-side measuring tools, cross-built in the build image.
#   build-tools.sh <libGPUCounters checkout>   -> build/probe/
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=../../scripts/build-container.sh
source "$here/scripts/build-container.sh"
mkdir -p "$here/build/probe"
# shellcheck disable=SC2016
mediabox_build_run -v "$1:/gpc:ro" -- '
  cd /work
  aarch64-linux-gnu-g++ -std=c++17 -O2 -static-libstdc++ -static-libgcc -o build/probe/mali-counters \
    -I/gpc/hwcpipe/include -I/gpc/hwcpipe/src -I/gpc/backend/device/include -I/gpc/backend/device/src \
    tools/spatial-bench/mali-counters.cpp \
    $(find /gpc/hwcpipe/src /gpc/backend/device/src -name "*.cpp" ! -path "*test*") -lpthread'
