#!/bin/sh
# We expect to receive: <zone name> <zone serial> <approval token>
set -euo pipefail -x

echo "Hook invoked with $@"

ZONE_NAME="$1"
ZONE_SERIAL="$2"
APPROVAL_TOKEN="$3"

dig +noall +onesoa +answer @127.0.0.1 -p 8057 example.com AXFR | dnssec-verify -o example.com /dev/stdin /tmp/keys/ || {
    wget -qO- http://127.0.0.1:8080/rs2/${ZONE_NAME}/${ZONE_SERIAL}/reject/${APPROVAL_TOKEN}
    exit 0
}

wget -qO- http://127.0.0.1:8080/rs2/${ZONE_NAME}/${ZONE_SERIAL}/approve/${APPROVAL_TOKEN}