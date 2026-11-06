#!/bin/sh
# An old Claude Code for onicode-harness tests: answers `--version` with
# 2.1.200, then plays fake-claude.sh. Flags newer CLIs know must not reach it.
case "${1:-}" in
  --version)
    echo "2.1.200 (Claude Code)"
    exit 0
    ;;
  *) ;;
esac
exec "$(dirname "$0")/fake-claude.sh" "$@"
