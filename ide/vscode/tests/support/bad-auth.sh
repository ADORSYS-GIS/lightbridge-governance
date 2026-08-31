#!/usr/bin/env bash
# Stands in for an unusable session: nothing on stdout, non-zero exit.
echo "no cached session for this issuer/client; run 'governance-auth login' first" >&2
exit 1
