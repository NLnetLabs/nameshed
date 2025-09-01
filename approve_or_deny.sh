#!/usr/bin/env bash
# We expect to receive: <zone name> <zone serial> <approval token>
set -euo pipefail -x

echo "Hook invoked with $*"

ZONE_NAME="$1"
ZONE_SERIAL="$2"
APPROVAL_TOKEN="$3"

wget -qO- "http://127.0.0.1:8950/_unit/rs/approve/${APPROVAL_TOKEN}?zone=${ZONE_NAME}&serial=${ZONE_SERIAL}"
