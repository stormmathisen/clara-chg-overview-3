#!/bin/bash
set -euo pipefail

# Derive version from git: use describe (tag+commits) or fall back to short hash
VERSION=$(git describe --tags --dirty --always 2>/dev/null || git rev-parse --short HEAD 2>/dev/null || echo "unknown")
IMAGE="docker.dsastvx10.dl.ac.uk/clara-chg-overview-3"

docker build . -t "${IMAGE}:${VERSION}" -t "${IMAGE}:latest"

# Optional first arg selects the charge config (bare name -> /app/config/<name>, or a
# full path). Default is baked into the image. e.g.:  ./build.sh charge_devices.dummy.yaml
CONFIG_ENV=()
if [[ $# -ge 1 ]]; then
  case "$1" in
    */*) CONFIG_ENV=(-e "CHARGE_CONFIG=$1") ;;
    *)   CONFIG_ENV=(-e "CHARGE_CONFIG=/app/config/$1") ;;
  esac
fi

# Stop and remove any existing container, then start with the version-tagged image
docker rm -f clara-chg-overview 2>/dev/null || true
docker run --name clara-chg-overview --network host "${CONFIG_ENV[@]}" "${IMAGE}:${VERSION}"
