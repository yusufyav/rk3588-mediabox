#!/usr/bin/env bash
# Reads the attached Sony BRAVIA's own view of its state over the built-in
# IP-control REST API, so that "did the TV enter HDR" can be answered from the
# sink rather than only from the source or from a person watching the screen.
#
#   scripts/tv-state.sh [label]
#
# Only getters are called. Nothing on the TV is changed.
#
# getPictureQualitySettings is the useful one: BRAVIA keeps a separate set of
# picture values per input and per SDR/HDR profile, so the values and
# availability flags reported under the same pictureMode differ between an SDR
# and an HDR signal. There is no documented "is the incoming signal HDR"
# getter on this generation, which is why this is captured as a differential
# rather than read as a single flag.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"

: "${MEDIABOX_TV_HOST:=10.27.27.51}"
label="${1:-}"

sony() {
  local service="$1" body="$2"
  curl -s --max-time 8 -X POST "http://$MEDIABOX_TV_HOST/sony/$service" \
    -H 'Content-Type: application/json' -d "$body"
}

printf '{\n  "label": "%s",\n  "tv_host": "%s",\n  "captured_utc": "%s",\n' \
  "$label" "$MEDIABOX_TV_HOST" "$(date -u +%FT%TZ)"

printf '  "interface": %s,\n' \
  "$(sony system '{"method":"getInterfaceInformation","id":1,"params":[],"version":"1.0"}')"
printf '  "power": %s,\n' \
  "$(sony system '{"method":"getPowerStatus","id":2,"params":[],"version":"1.0"}')"
printf '  "external_inputs": %s,\n' \
  "$(sony avContent '{"method":"getCurrentExternalInputsStatus","id":3,"params":[],"version":"1.1"}')"
printf '  "picture_quality": %s\n}\n' \
  "$(sony video '{"method":"getPictureQualitySettings","id":4,"params":[{"target":""}],"version":"1.0"}')"
