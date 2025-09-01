#!/usr/bin/env bash

set -eo pipefail
set -x

case $1 in
  post-install)
    #echo -e "\nCASCADE VERSION:"
    #VER=$(cascade --version)
    #echo $VER

    echo -e "\nCASCADE CONF:"
    cat /etc/cascade/cascade.conf

    echo -e "\nCASCADE SERVICE STATUS:"
    systemctl status cascade || true

    #echo -e "\nCASCADE MAN PAGE (first 20 lines only):"
    #man -P cat cascade | head -n 20 || true
    ;;

  post-upgrade)
    #echo -e "\nCASCADE VERSION:"
    #cascade --version
    
    echo -e "\nCASCADE CONF:"
    cat /etc/cascade/cascade.conf
    
    echo -e "\nCASCADE SERVICE STATUS:"
    systemctl status cascade || true
    
    #echo -e "\nCASCADE MAN PAGE:"
    #man -P cat cascade
    ;;
esac
