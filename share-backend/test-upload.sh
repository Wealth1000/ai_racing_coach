#!/usr/bin/env bash
# Send a share bundle to the receiver exactly the way the coach's
# "Send to author" does — the same headers, the same body — so a
# successful curl means a successful Send.
#
# usage: ./test-upload.sh <endpoint> <bundle>
#   ./test-upload.sh https://coach-share.<subdomain>.workers.dev \
#       data/share/share_monza_20260903_141500.json.gz
set -euo pipefail

endpoint=${1:?usage: test-upload.sh <endpoint> <bundle>}
bundle=${2:?usage: test-upload.sh <endpoint> <bundle>}

curl -sS -X POST "$endpoint" \
    -H "Content-Type: application/gzip" \
    -H "X-Coach-Share-Schema: 1" \
    --data-binary "@$bundle"
echo
