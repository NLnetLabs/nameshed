#!/bin/sh
# We expect to receive: <zone name> <zone serial> <approval token>
set -euo pipefail -x

echo "Hook invoked with $@"

ZONE_NAME="$1"
ZONE_SERIAL="$2"
APPROVAL_TOKEN="$3"

wget -qO- http://127.0.0.1:8080/rs/${ZONE_NAME}/${ZONE_SERIAL}/approve/${APPROVAL_TOKEN}