#!/usr/bin/env bash
# Installs dune and dunx from the j8 GitHub releases.
#   curl -fsSL https://j8.app238.com/install | bash
#   curl -fsSL https://j8.app238.com/install | bash -s v0.5.0
# DUNE_INSTALL sets the install root (default ~/.dune, binaries in its bin/);
# DUNE_RELEASES sets where releases are downloaded from (default the j8 GitHub releases).
set -euo pipefail

RELEASES=${DUNE_RELEASES:-https://github.com/0x00-Community/j8/releases}
ROOT=${DUNE_INSTALL:-$HOME/.dune}
BIN=$ROOT/bin
TAG=${1:-}

if [[ -t 1 ]]; then
  RED=$'\033[0;31m' DIM=$'\033[0;2m' GREEN=$'\033[0;32m' BOLD=$'\033[1m' RESET=$'\033[0m'
else
  RED='' DIM='' GREEN='' BOLD='' RESET=''
fi
error() { echo "${RED}error${RESET}: $*" >&2; exit 1; }
info() { echo "${DIM}$*${RESET}"; }
success() { echo "${GREEN}$*${RESET}"; }

command -v curl >/dev/null || error "curl is required to install dune"
command -v tar >/dev/null || error "tar is required to install dune"

case $(uname -s) in
  Linux) os=linux ;;
  Darwin) os=darwin ;;
  MINGW* | MSYS* | CYGWIN*) error "on Windows run: powershell -c \"irm https://j8.app238.com/install.ps1 | iex\"" ;;
  *) error "unsupported OS: $(uname -s)" ;;
esac

case $(uname -m) in
  x86_64 | amd64) cpu=x64 ;;
  aarch64 | arm64) cpu=arm64 ;;
  *) error "unsupported CPU: $(uname -m)" ;;
esac

# A shell running under Rosetta 2 reports x86_64 on Apple silicon: install the native build
if [[ $os == darwin && $cpu == x64 && $(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0) == 1 ]]; then
  cpu=arm64
  info "Rosetta 2 detected: installing the arm64 build"
fi

if [[ $os == linux ]] && { ls /lib/ld-musl-* >/dev/null 2>&1 || ldd --version 2>&1 | grep -qi musl; }; then
  error "musl Linux has no prebuilt dune: build it with cargo install j8dune"
fi

target=dune-$os-$cpu
archive=$target.tar.gz
if [[ -z $TAG ]]; then
  url=$RELEASES/latest/download
else
  [[ $TAG == v* ]] || TAG=v$TAG
  url=$RELEASES/download/$TAG
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

info "Downloading $url/$archive"
curl --fail --location --progress-bar --output "$tmp/$archive" "$url/$archive" ||
  error "failed to download $archive${TAG:+ ($TAG)}; is there a release for $os-$cpu$libc?"

if curl --fail --silent --location --output "$tmp/SHA256SUMS" "$url/SHA256SUMS"; then
  expected=$(awk -v f="$archive" '$2 == f || $2 == "*"f { print $1 }' "$tmp/SHA256SUMS")
  if command -v sha256sum >/dev/null; then
    actual=$(sha256sum "$tmp/$archive" | awk '{ print $1 }')
  elif command -v shasum >/dev/null; then
    actual=$(shasum -a 256 "$tmp/$archive" | awk '{ print $1 }')
  else
    actual=
  fi
  if [[ -n $expected && -n $actual && $expected != "$actual" ]]; then
    error "checksum mismatch for $archive"
  fi
fi

tar -xzf "$tmp/$archive" -C "$tmp" || error "failed to extract $archive"
mkdir -p "$BIN"
mv -f "$tmp/$target/dune" "$BIN/dune"
chmod +x "$BIN/dune"
ln -sf dune "$BIN/dunx"

version=$("$BIN/dune" --version 2>/dev/null || true)
success "${version:-dune} was installed to ${BOLD}$BIN/dune${RESET}${GREEN} (and dunx)"

if command -v dune >/dev/null && [[ $(command -v dune) == "$BIN/dune" ]]; then
  echo "Run ${BOLD}dune --help${RESET} to get started"
  exit 0
fi

tilde_bin=${BIN/#$HOME/\~}
quoted_root=${ROOT/#$HOME/\$HOME}
refresh=
case $(basename "${SHELL:-}") in
  fish)
    config=$HOME/.config/fish/config.fish
    lines=("set --export DUNE_INSTALL \"$quoted_root\"" "set --export PATH \$DUNE_INSTALL/bin \$PATH")
    refresh="source ${config/#$HOME/\~}"
    ;;
  zsh)
    config=${ZDOTDIR:-$HOME}/.zshrc
    lines=("export DUNE_INSTALL=\"$quoted_root\"" "export PATH=\"\$DUNE_INSTALL/bin:\$PATH\"")
    refresh="exec \$SHELL"
    ;;
  bash)
    config=$HOME/.bashrc
    [[ $os == darwin && -f $HOME/.bash_profile ]] && config=$HOME/.bash_profile
    lines=("export DUNE_INSTALL=\"$quoted_root\"" "export PATH=\"\$DUNE_INSTALL/bin:\$PATH\"")
    refresh="source ${config/#$HOME/\~}"
    ;;
  *)
    config=
    ;;
esac

if [[ -n $config ]] && { [[ -w $config ]] || [[ ! -e $config && -w $(dirname "$config") ]]; }; then
  if ! grep -qs "DUNE_INSTALL" "$config"; then
    mkdir -p "$(dirname "$config")"
    { echo; echo "# dune"; printf '%s\n' "${lines[@]}"; } >>"$config"
    info "Added \"$tilde_bin\" to \$PATH in \"${config/#$HOME/\~}\""
  fi
  echo
  echo "To get started, run:"
  echo "  ${BOLD}$refresh${RESET}"
  echo "  ${BOLD}dune --help${RESET}"
else
  echo
  echo "Add dune to your PATH manually:"
  echo "  ${BOLD}export DUNE_INSTALL=\"$quoted_root\"${RESET}"
  echo "  ${BOLD}export PATH=\"\$DUNE_INSTALL/bin:\$PATH\"${RESET}"
fi
