#!/bin/sh
# release-docs.sh — stamp the release tag into the install URLs before tagging.
#
# The release operator runs `scripts/release-docs.sh <tag>` (for example
# `scripts/release-docs.sh v3.3.0`) on main BEFORE tagging: it replaces the
# `{{TAG}}` placeholder in README.md and docs/*.md with the tag, so the
# install command serves the tagged, immutable install script instead of
# main. Nothing calls this script automatically (there is no release
# checklist); run it by hand and commit the result before tagging.
set -eu

if [ $# -ne 1 ] || [ -z "${1:-}" ]; then
  echo "usage: $0 <tag>  (for example: $0 v3.3.0)" >&2
  exit 1
fi
TAG="$1"

found=0
for file in README.md docs/*.md; do
  if grep -q "{{TAG}}" "$file" 2>/dev/null; then
    found=1
    sed "s|{{TAG}}|$TAG|g" "$file" > "$file.stamp.tmp" && mv "$file.stamp.tmp" "$file"
    echo "stamped $file"
  fi
done
if [ "$found" = "0" ]; then
  echo "no {{TAG}} placeholder found in README.md or docs/*.md" >&2
  exit 1
fi
