#!/usr/bin/env bash
# The journey benchmark with recorded network inputs: record, run, compare. Usage and columns: tools/bench/journeys.py.
exec python3 -I "$(dirname "$0")/journeys.py" "$@"
