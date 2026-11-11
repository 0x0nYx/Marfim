#!/bin/sh
# PreToolUse hook example: block any tool input that carries a literal
# secret-looking assignment. Hooks receive JSON on stdin and deny with a
# non-zero exit + reason on stdout. Keep hooks fast (they gate every call).
input=$(cat)
case "$input" in
  *api_key*|*apikey*|*AWS_SECRET*|*"PRIVATE KEY"*|*ghp_*|*sk-*)
    echo "refusing: input mentions a secret-shaped token; use an env var reference instead"
    exit 2
    ;;
esac
exit 0
