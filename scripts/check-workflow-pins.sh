#!/usr/bin/env bash
set -euo pipefail

fail=0
for file in .github/workflows/*.yml; do
  lineno=0
  while IFS= read -r line || [ -n "$line" ]; do
    lineno=$((lineno + 1))
    case "$line" in
      *uses:*)
        value=$(printf '%s\n' "$line" | sed -e 's/^[^:]*uses:[[:space:]]*//' -e "s/^['\"]//" -e "s/['\"].*\$//" -e 's/[[:space:]]*#.*$//' -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')
        case "$value" in
          ""|./*) continue ;;
        esac
        ref=${value##*@}
        if [[ "$value" == *"@"* ]] && [[ "$ref" =~ ^[0-9a-f]{40}$ ]]; then
          continue
        fi
        printf '%s:%s: %s is not pinned to a 40-hex commit SHA\n' "$file" "$lineno" "$value" >&2
        fail=1
        ;;
    esac
  done < "$file"
done

if [ "$fail" -ne 0 ]; then
  exit 1
fi
printf 'workflow pins ok\n'
