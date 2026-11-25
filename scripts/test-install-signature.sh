#!/bin/sh
# test-install-signature.sh — signature paths of scripts/install.sh with a
# stub curl and stub minisign on PATH (no network, no real minisign needed).
#
# Cases: valid signature, missing minisign (fatal with hint),
# CANOPY_ALLOW_UNSIGNED=1 (warns, skips), wrong trusted comment (fatal),
# mismatched checksum (fatal).
set -eu

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
INSTALL_SH="$SCRIPT_DIR/install.sh"
VERSION="9.9.9-test"
FAILURES=0

# --- platform target, same mapping as install.sh ---
OS="$(uname -s)"
case "$OS" in
  Linux*) OS_TARGET="unknown-linux-gnu" ;;
  Darwin*) OS_TARGET="apple-darwin" ;;
esac
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64) ARCH_TARGET="x86_64" ;;
  arm64|aarch64) ARCH_TARGET="aarch64" ;;
esac
TARGET="${ARCH_TARGET}-${OS_TARGET}"
ARCHIVE="canopy-v${VERSION}-${TARGET}.tar.gz"

# --- isolated tool dir: real tools only, never the real minisign/curl ---
link_tools() {
  dir="$1"
  for tool in uname mktemp rev cut grep head awk sha256sum shasum tar gzip mkdir mv chmod rm printf cp; do
    if command -v "$tool" >/dev/null 2>&1; then
      ln -sf "$(command -v "$tool")" "$dir/$tool"
    fi
  done
}

write_curl_stub() {
  dir="$1"
  cat > "$dir/curl" <<'STUB'
#!/bin/sh
# Serves fixture files by URL suffix; prints an HTTP code like -w '%{http_code}'.
OUT=""
URL=""
PREV=""
for a in "$@"; do
  if [ "$PREV" = "-o" ]; then OUT="$a"; PREV=""; continue; fi
  case "$a" in
    -o) PREV="-o" ;;
    -*) ;;
    *) URL="$a" ;;
  esac
done
case "$URL" in
  *SHA256SUMS.txt.minisig) cp "$FIXTURE_DIR/minisig" "$OUT" || exit 22; printf '200' ;;
  *SHA256SUMS.txt) cp "$FIXTURE_DIR/sums" "$OUT" || exit 22; printf '200' ;;
  *.tar.gz) cp "$FIXTURE_DIR/archive.tar.gz" "$OUT" || exit 22; printf '200' ;;
  *) echo "curl stub: unexpected URL $URL" >&2; exit 22 ;;
esac
STUB
  chmod +x "$dir/curl"
}

write_minisign_stub() {
  dir="$1"
  cat > "$dir/minisign" <<'STUB'
#!/bin/sh
echo "Signature and comment signature verified"
exit 0
STUB
  chmod +x "$dir/minisign"
}

# make_fixture <dir> <comment-tag> <sha-override-or-empty>
make_fixture() {
  dir="$1"
  comment_tag="$2"
  sha_override="${3:-}"
  mkdir -p "$dir/pkg"
  printf '#!/bin/sh\necho "canopy %s"\n' "$VERSION" > "$dir/pkg/canopy"
  chmod +x "$dir/pkg/canopy"
  tar czf "$dir/archive.tar.gz" -C "$dir/pkg" canopy
  if [ -n "$sha_override" ]; then
    sha="$sha_override"
  else
    sha=$(sha256sum "$dir/archive.tar.gz" | awk '{ print $1 }')
  fi
  printf '%s  %s\n' "$sha" "$ARCHIVE" > "$dir/sums"
  printf 'untrusted comment: test signature\nAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==\ntrusted comment: canopy %s\nBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB==\n' "$comment_tag" > "$dir/minisig"
}

# run_case <name> <with-minisign:0/1> <allow-unsigned:0/1> <comment-tag> <sha-override>
run_case() {
  name="$1"
  with_minisign="$2"
  allow_unsigned="$3"
  comment_tag="$4"
  sha_override="${5:-}"
  case_dir="$(mktemp -d)"
  mkdir -p "$case_dir/home" "$case_dir/install" "$case_dir/bin" "$case_dir/fixture"
  link_tools "$case_dir/bin"
  write_curl_stub "$case_dir/bin"
  if [ "$with_minisign" = "1" ]; then
    write_minisign_stub "$case_dir/bin"
  fi
  make_fixture "$case_dir/fixture" "$comment_tag" "$sha_override"
  if [ "$allow_unsigned" = "1" ]; then
    allow_env="CANOPY_ALLOW_UNSIGNED=1"
  else
    allow_env="CANOPY_ALLOW_UNSIGNED="
  fi
  SH_BIN="$(command -v sh)"
  set +e
  output=$(env -i PATH="$case_dir/bin" HOME="$case_dir/home" \
    INSTALL_DIR="$case_dir/install" VERSION="$VERSION" \
    FIXTURE_DIR="$case_dir/fixture" $allow_env \
    "$SH_BIN" "$INSTALL_SH" 2>&1)
  status=$?
  set -e
  printf '%s|%s|%s\n' "$name" "$status" "$case_dir"
  printf '%s\n' "$output" > "$case_dir/output.txt"
}

check() {
  name="$1"
  expected_status="$2"
  expected_text="$3"
  result="$4"
  status="${result%%|*}"
  rest="${result#*|}"
  got_status="${rest%%|*}"
  case_dir="${rest#*|}"
  output=$(cat "$case_dir/output.txt")
  rm -rf "$case_dir"
  if [ "$got_status" != "$expected_status" ]; then
    printf 'FAIL %s: exit %s, want %s\n%s\n' "$name" "$got_status" "$expected_status" "$output"
    FAILURES=$((FAILURES + 1))
    return
  fi
  case "$output" in
    *"$expected_text"*) printf 'PASS %s\n' "$name" ;;
    *)
      printf 'FAIL %s: missing %s\n%s\n' "$name" "$expected_text" "$output"
      FAILURES=$((FAILURES + 1))
      ;;
  esac
}

TAG="v$VERSION"
check "valid" 0 "checksum" "$(run_case valid 1 0 "$TAG")"
check "no-minisign" 1 "minisign is required" "$(run_case no-minisign 0 0 "$TAG")"
check "allow-unsigned" 0 "CANOPY_ALLOW_UNSIGNED=1, skipping signature verification" "$(run_case allow-unsigned 0 1 "$TAG")"
check "bad-comment" 1 "Trusted comment mismatch" "$(run_case bad-comment 1 0 "v0.0.0-evil")"
check "bad-sha256" 1 "Checksum mismatch" "$(run_case bad-sha256 1 0 "$TAG" "0000000000000000000000000000000000000000000000000000000000000000")"

if [ "$FAILURES" = "0" ]; then
  echo "ALL INSTALL SIGNATURE TESTS PASSED"
else
  echo "$FAILURES CASE(S) FAILED"
  exit 1
fi
