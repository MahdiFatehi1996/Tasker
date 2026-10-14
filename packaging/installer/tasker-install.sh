#!/usr/bin/env sh
#
# tasker installer — POSIX sh, no dependencies beyond curl or wget.
#
#   curl --proto '=https' --tlsv1.2 -LsSf \
#     https://github.com/MahdiFatehi1996/Tasker/releases/latest/download/tasker-install.sh | sh
#
# It resolves the release, downloads the archive built for this machine,
# verifies it against SHA256SUMS.txt, and unpacks the binary into
# ~/.local/bin. It never edits your shell profile; if that directory is not
# on PATH it prints the line to add.

set -eu

REPO="MahdiFatehi1996/Tasker"
DEFAULT_DIR="${HOME}/.local/bin"

log() { printf '%s\n' "$*" >&2; }
die() { printf 'tasker-install: %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<EOF
Usage: tasker-install.sh [options]

Options:
  --version <tag>  Install a specific release, e.g. --version v0.3.2.
                   Defaults to the latest release. TASKER_VERSION does the same.
  --dir <path>     Install into <path> instead of $DEFAULT_DIR.
                   TASKER_INSTALL_DIR does the same.
  -h, --help       Show this help.

Exit status: 0 on success, 1 on failure.
EOF
}

version=""
destdir=""

while [ $# -gt 0 ]; do
  case "$1" in
    --version) [ $# -ge 2 ] || die "--version needs a value"; version="$2"; shift 2 ;;
    --version=*) version="${1#*=}"; shift ;;
    --dir) [ $# -ge 2 ] || die "--dir needs a value"; destdir="$2"; shift 2 ;;
    --dir=*) destdir="${1#*=}"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown option: $1 (try --help)" ;;
  esac
done

[ -n "$version" ] || version="${TASKER_VERSION:-}"
[ -n "$destdir" ] || destdir="${TASKER_INSTALL_DIR:-$DEFAULT_DIR}"

workdir=""
cleanup() {
  if [ -n "$workdir" ]; then
    rm -rf "$workdir" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

workdir="$(mktemp -d 2>/dev/null || mktemp -d -t tasker)"

# --- fetch a URL to a file, or a URL's body to stdout -------------------------

fetch_file() { # url dest
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$2" "$1"
  else
    die "need curl or wget to download files"
  fi
}

fetch_body() { # url
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO- "$1"
  else
    die "need curl or wget to download files"
  fi
}

# --- which release ------------------------------------------------------------

if [ -z "$version" ]; then
  log "resolving the latest release of tasker"
  if command -v curl >/dev/null 2>&1; then
    # Follow the "latest" redirect and read the tag out of the final URL,
    # which avoids the GitHub API's unauthenticated rate limit.
    effective="$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
      "https://github.com/$REPO/releases/latest" 2>/dev/null || true)"
    version="${effective##*/}"
  fi
  if [ -z "$version" ]; then
    body="$(fetch_body "https://api.github.com/repos/$REPO/releases/latest" || true)"
    version="$(printf '%s' "$body" | tr -d ' \n' | sed -n 's/.*"tag_name":"\([^"]*\)".*/\1/p')"
  fi
  [ -n "$version" ] || die "could not determine the latest release tag (use --version, or set TASKER_VERSION)"
fi

# Tags are published as vX.Y.Z; accept a bare version too.
case "$version" in
  v*) ;;
  *) version="v$version" ;;
esac

# --- which binary -------------------------------------------------------------

os="$(uname -s)"
arch="$(uname -m)"

case "$arch" in
  x86_64|amd64) arch="x86_64" ;;
  aarch64|arm64) arch="aarch64" ;;
  *) die "unsupported CPU architecture: $arch" ;;
esac

case "$os" in
  Linux)
    [ "$arch" = "x86_64" ] || die "no prebuilt Linux binary for $arch; build from source instead"
    target="${arch}-unknown-linux-gnu"
    archive="tasker-${target}.tar.gz"
    binname="tasker"
    ;;
  Darwin)
    target="${arch}-apple-darwin"
    archive="tasker-${target}.tar.gz"
    binname="tasker"
    ;;
  MINGW*|MSYS*|CYGWIN*)
    [ "$arch" = "x86_64" ] || die "no prebuilt Windows binary for $arch"
    target="${arch}-pc-windows-msvc"
    archive="tasker-${target}.zip"
    binname="tasker.exe"
    ;;
  *)
    die "unsupported operating system: $os"
    ;;
esac

base="https://github.com/$REPO/releases/download/$version"
log "installing tasker $version ($target)"

# --- download and verify ------------------------------------------------------

fetch_file "$base/$archive" "$workdir/$archive" \
  || die "download failed: $base/$archive"
fetch_file "$base/SHA256SUMS.txt" "$workdir/SHA256SUMS.txt" \
  || die "download failed: $base/SHA256SUMS.txt"

actual=""
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$workdir/$archive" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$workdir/$archive" | awk '{print $1}')"
elif command -v sha256 >/dev/null 2>&1; then
  actual="$(sha256 -q "$workdir/$archive")"
fi

if [ -n "$actual" ]; then
  expected="$(awk -v f="$archive" '$2 == f { print $1 }' "$workdir/SHA256SUMS.txt")"
  [ -n "$expected" ] || die "$archive is missing from SHA256SUMS.txt"
  [ "$actual" = "$expected" ] \
    || die "checksum mismatch for $archive: expected $expected, got $actual"
  log "checksum verified"
else
  log "warning: no sha256 tool found, skipping checksum verification"
fi

# --- unpack and install -------------------------------------------------------

# Zip extraction differs a lot between environments: Git for Windows and MSYS2
# ship unzip, bsdtar does the job on macOS, and bare MSYS2 has GNU tar, which
# cannot read zip at all. Try them in that order and fall back to PowerShell.
extract_zip() { # archive dest
  if command -v unzip >/dev/null 2>&1; then
    unzip -q -o "$1" -d "$2" && return 0
  fi
  if command -v bsdtar >/dev/null 2>&1; then
    bsdtar -xf "$1" -C "$2" && return 0
  fi
  if tar -xf "$1" -C "$2" 2>/dev/null; then
    return 0
  fi
  if command -v powershell.exe >/dev/null 2>&1; then
    archive_win="$1"
    dest_win="$2"
    if command -v cygpath >/dev/null 2>&1; then
      archive_win="$(cygpath -w "$1")"
      dest_win="$(cygpath -w "$2")"
    fi
    if powershell.exe -NoProfile -Command \
      "Expand-Archive -LiteralPath '$archive_win' -DestinationPath '$dest_win' -Force" \
      >/dev/null 2>&1; then
      return 0
    fi
  fi
  return 1
}

mkdir -p "$workdir/extracted"
case "$archive" in
  *.tar.gz) tar -xzf "$workdir/$archive" -C "$workdir/extracted" ;;
  *.zip)
    extract_zip "$workdir/$archive" "$workdir/extracted" \
      || die "could not extract $archive (try installing unzip)"
    ;;
  *) die "unexpected archive name: $archive" ;;
esac

source_bin="$workdir/extracted/$binname"
[ -f "$source_bin" ] || die "the archive did not contain $binname"

mkdir -p "$destdir" || die "cannot create $destdir"
cp "$source_bin" "$destdir/$binname" || die "cannot write to $destdir"
chmod 755 "$destdir/$binname" 2>/dev/null || true

# --- smoke test ---------------------------------------------------------------

testdir="$(mktemp -d 2>/dev/null || mktemp -d -t tasker-test)"
if "$destdir/$binname" --db "$testdir/tasks.db" capabilities --json >/dev/null 2>&1; then
  log "verified $destdir/$binname runs"
else
  log "warning: installed $destdir/$binname but could not run it here"
  log "         try: tasker --help, or open an issue at https://github.com/$REPO/issues"
fi
rm -rf "$testdir"

# --- report -------------------------------------------------------------------

case ":${PATH:-}:" in
  *":$destdir:"*)
    log "tasker installed to $destdir"
    ;;
  *)
    log "tasker installed to $destdir"
    log "that directory is not on PATH; add it by putting this in your shell profile:"
    log "    export PATH=\"$destdir:\$PATH\""
    ;;
esac
