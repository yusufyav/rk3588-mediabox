#!/usr/bin/env bash
# The media core's own suite. No hardware, no appliance, no network.
#
# Kept separate from tests/run-mediaboxd-tests.sh so the media core can be run
# on its own — including from a staging directory on the appliance, where it is
# copied without the rest of the repository.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$here"
exec python3 -m unittest discover -s media/tests -t . -p 'test_*.py' "$@"
