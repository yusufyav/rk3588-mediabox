#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$here"
python3 -m unittest discover -s tests -p 'test_mediaboxd.py' -v
