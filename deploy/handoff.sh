#!/bin/sh
# Hand a packaged build over to sysiphe (the deploy agent on minix): upload the tarball and the brief, then prompt it.
# Usage: deploy/handoff.sh dist/tinynantes-context-<version>.tar.gz dist/BRIEF-<x>.md
set -eu
tar="$1"; brief="$2"
scp "$tar" "$brief" minix:Work/tinynantes-dist/
# no --wait: herdr's status wait times out long before a deploy ends, and reads as a failure although the prompt
# was delivered. Follow the rollout on https://tinynantes.fr/version.json instead.
ssh minix "~/.nix-profile/bin/herdr agent prompt sysiphe \"\$(cat ~/Work/tinynantes-dist/$(basename "$brief"))\"" >/dev/null
echo "brief delivered to sysiphe: it will ask for the owner's go-ahead in its herdr tab on minix"
