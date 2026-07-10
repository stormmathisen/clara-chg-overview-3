#!/bin/bash
set -euo pipefail

# Derive version from git: use describe (tag+commits) or fall back to short hash
VERSION=$(git describe --tags --dirty --always 2>/dev/null || git rev-parse --short HEAD 2>/dev/null || echo "unknown")
IMAGE="docker.dsastvx10.dl.ac.uk/clara-chg-overview-3"

docker build . -t "${IMAGE}:${VERSION}" -t "${IMAGE}:latest"

# Stop and remove any existing container, then start with the version-tagged image
docker rm -f clara-chg-overview 2>/dev/null || true
docker run --name clara-chg-overview --network host "${IMAGE}:${VERSION}"
