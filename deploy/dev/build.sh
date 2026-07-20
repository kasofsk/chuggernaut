#!/bin/sh
# Build the dev images and extract the linux chuggernaut-channel binary
# (deploy/dev/out/chuggernaut-channel → the dispatcher's CHANNEL_BINARY).
set -ex
cd "$(dirname "$0")"

docker build -f Dockerfile.ssh --target artifacts --output type=local,dest=out ../..
docker build -f Dockerfile.ssh -t chuggernaut/ssh:dev ../..
docker build -f Dockerfile.agent -t chuggernaut/agent:dev .
