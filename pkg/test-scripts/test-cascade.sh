#!/usr/bin/env bash

set -eo pipefail
set -x

case $1 in
  post-install|post-upgrade)
    echo -e "\nCASCADE VERSION:"
    cascade --version

    echo -e "\nCASCADED VERSION:"
    cascaded --version

    echo -e "\nCASCADED CONF:"
    cat /etc/cascade/cascaded.conf

    echo -e "\nCASCADED SERVICE STATUS:"
    systemctl status cascaded || true

    #echo -e "\nCASCADE MAN PAGE (first 20 lines only):"
    #man -P cat cascade | head -n 20 || true

    #echo -e "\nCASCADED MAN PAGE (first 20 lines only):"
    #man -P cat cascaded | head -n 20 || true
    ;;
esac
