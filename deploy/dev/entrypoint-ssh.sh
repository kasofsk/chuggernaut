#!/bin/sh
# Generate a host key on first boot (persisted via the ssh-hostkeys volume so
# agent containers with StrictHostKeyChecking=no aside, humans keep one key).
set -e
mkdir -p /etc/ssh/hostkeys
[ -f /etc/ssh/hostkeys/ssh_host_ed25519_key ] \
  || ssh-keygen -t ed25519 -N "" -f /etc/ssh/hostkeys/ssh_host_ed25519_key
exec /usr/sbin/sshd -D -e
