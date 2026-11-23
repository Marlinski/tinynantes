#!/bin/sh
# Hand a packaged build over to sysiphe (the deploy agent on minix): upload the tarball and the brief, then prompt it.
# Usage: deploy/handoff.sh dist/tinynantes-context-<version>.tar.gz dist/BRIEF-<x>.md
set -eu
tar="$1"; brief="$2"
scp "$tar" "$brief" minix:Work/tinynantes-dist/
ssh minix "~/.nix-profile/bin/herdr agent prompt sysiphe \"\$(cat ~/Work/tinynantes-dist/$(basename "$brief"))\" --wait --timeout 3600"
