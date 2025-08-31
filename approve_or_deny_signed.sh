#!/bin/bash
# We expect to receive: <zone name> <zone serial> <approval token>
set -euo pipefail -x

echo "Hook invoked with $*"

ZONE_NAME="$1"
ZONE_SERIAL="$2"
APPROVAL_TOKEN="$3"

dig +noall +onesoa +answer @127.0.0.1 -p 8057 "${ZONE_NAME}" AXFR | dnssec-verify -o "${ZONE_NAME}" /dev/stdin /tmp/keys/ || {
    wget -qO- "http://127.0.0.1:8950/_unit/rs2/reject/${APPROVAL_TOKEN}?zone=${ZONE_NAME}&serial=${ZONE_SERIAL}"
    exit 0
}

wget -qO- "http://127.0.0.1:8950/_unit/rs2/approve/${APPROVAL_TOKEN}?zone=${ZONE_NAME}&serial=${ZONE_SERIAL}"
