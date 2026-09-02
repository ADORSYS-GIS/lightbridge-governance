#!/usr/bin/env bash
# Stands in for a working `governance-auth token`, and records that it ran, so
# a test can assert how many process spawns the extension actually caused.
[ -n "${LB_SPAWN_LOG:-}" ] && echo "spawn" >>"$LB_SPAWN_LOG"
echo "fake-access-token-value"
