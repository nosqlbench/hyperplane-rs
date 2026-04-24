#!/usr/bin/env bash
# Copyright (c) Jonathan Shook
# SPDX-License-Identifier: Apache-2.0
#
# Rebuild every .png next to its .mmd source under docs/design/diagrams/.
# Uses the official mermaid-cli docker image so we don't depend on a local
# Chrome + headless-rendering stack. PNG output is used (not SVG) so the
# diagrams render in every markdown viewer, not just those that support SVG.
#
# Usage:
#   bash docs/design/build-diagrams.sh            # rebuild all
#   bash docs/design/build-diagrams.sh --check    # fail if any .png would change
#   bash docs/design/build-diagrams.sh path/to/one.mmd   # rebuild just one

set -euo pipefail

DIAGRAM_ROOT="$(cd "$(dirname "$0")" && pwd)/diagrams"
IMAGE="minlag/mermaid-cli:latest"
SCALE="2"   # 2x resolution for crispness in markdown viewers
MODE="${1:-build}"

render_one() {
    local mmd="$1"
    local dir
    dir="$(dirname "$mmd")"
    local rel
    rel="$(basename "$mmd")"

    docker run --rm \
        -u "$(id -u):$(id -g)" \
        -v "$dir:/data" \
        "$IMAGE" \
        -i "/data/$rel" \
        -o "/data/${rel%.mmd}.png" \
        --scale "$SCALE" \
        > /dev/null
}

collect_sources() {
    find "$DIAGRAM_ROOT" -type f -name '*.mmd' | sort
}

case "$MODE" in
    --check)
        fail=0
        tmp="$(mktemp -d)"
        trap 'rm -rf "$tmp"' EXIT
        while IFS= read -r mmd; do
            png="${mmd%.mmd}.png"
            cp "$mmd" "$tmp/"
            basename="$(basename "$mmd")"
            docker run --rm \
                -u "$(id -u):$(id -g)" \
                -v "$tmp:/data" \
                "$IMAGE" \
                -i "/data/$basename" \
                -o "/data/${basename%.mmd}.png" \
                --scale "$SCALE" \
                > /dev/null
            if ! cmp -s "$tmp/${basename%.mmd}.png" "$png"; then
                echo "out of date: $png"
                fail=1
            fi
        done < <(collect_sources)
        exit $fail
        ;;
    build)
        count=0
        while IFS= read -r mmd; do
            echo "  rendering $(basename "$(dirname "$mmd")")/$(basename "$mmd")"
            render_one "$mmd"
            count=$((count + 1))
        done < <(collect_sources)
        echo "rendered $count diagram(s)"
        ;;
    *)
        # Treat as a single .mmd path
        render_one "$MODE"
        ;;
esac
