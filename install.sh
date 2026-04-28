#!/usr/bin/env bash
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
#  OmitFS — One-Line Installer (macOS + Linux)
#
#  Usage:
#    curl -sSf https://raw.githubusercontent.com/Panav-Payappagoudar/OmitFS/main/install.sh | sh
#
#  What this does:
#    1. Detects your OS and CPU architecture
#    2. Downloads the pre-built binary from GitHub Releases
#       (falls back to building from source if no release exists yet)
#    3. Installs the binary to ~/.local/bin/omitfs
#    4. Adds it to your PATH  (bash / zsh / fish)
#    5. Checks for Ollama (optional, needed only for "ask" / RAG mode)
#    6. Runs `omitfs init` — downloads embedding model weights (~80 MB, once)
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
set -euo pipefail

# ── Config ────────────────────────────────────────────────
REPO="Panav-Payappagoudar/OmitFS"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
OMITFS_DATA="${OMITFS_DATA:-$HOME/.omitfs_data}"

# ── Colors ────────────────────────────────────────────────
R='\033[0;31m' G='\033[0;32m' Y='\033[1;33m' B='\033[0;34m'
P='\033[0;35m' C='\033[0;36m' BOLD='\033[1m' NC='\033[0m'

banner() {
  printf "${P}${BOLD}"
  printf '  ╔══════════════════════════════════════════╗\n'
  printf '  ║          ██████╗ ███╗   ███╗██╗████████╗ ║\n'
  printf '  ║         ██╔═══██╗████╗ ████║██║╚══██╔══╝ ║\n'
  printf '  ║         ██║   ██║██╔████╔██║██║   ██║    ║\n'
  printf '  ║         ██║   ██║██║╚██╔╝██║██║   ██║    ║\n'
  printf '  ║         ╚██████╔╝██║ ╚═╝ ██║██║   ██║    ║\n'
  printf '  ║          ╚═════╝ ╚═╝     ╚═╝╚═╝   ╚═╝    ║\n'
  printf '  ║  Intent-Driven Local Semantic File System  ║\n'
  printf '  ╚══════════════════════════════════════════╝\n'
  printf "${NC}\n"
}

log()  { printf "${G}  ✓${NC}  %s\n" "$1"; }
info() { printf "${C}  →${NC}  %s\n" "$1"; }
warn() { printf "${Y}  ⚠${NC}  %s\n" "$1"; }
err()  { printf "${R}  ✗${NC}  %s\n" "$1" >&2; exit 1; }
step() { printf "\n${BOLD}${B}[$1]${NC} ${BOLD}%s${NC}\n" "$2"; }

# ── 1. Detect OS / Arch ───────────────────────────────────
detect_platform() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Linux*)  OS="linux" ;;
    Darwin*) OS="macos" ;;
    *)       err "Unsupported OS: $os — use install.ps1 on Windows." ;;
  esac

  case "$arch" in
    x86_64)        ARCH="x86_64" ;;
    arm64|aarch64) ARCH="arm64"  ;;
    *)             err "Unsupported architecture: $arch" ;;
  esac

  # Map to release artifact name
  if   [[ "$OS" == "macos"  && "$ARCH" == "arm64"  ]]; then BINARY="omitfs-macos-arm64"
  elif [[ "$OS" == "macos"  && "$ARCH" == "x86_64" ]]; then BINARY="omitfs-macos-x86_64"
  else BINARY="omitfs-linux-x86_64"; fi

  log "Platform: $OS / $ARCH"
}

# ── 2. Get download URL from latest GitHub release ────────
fetch_release_url() {
  info "Looking up latest GitHub release…"
  local api="https://api.github.com/repos/${REPO}/releases/latest"
  local json=""

  if command -v curl &>/dev/null; then
    json="$(curl -sSf "$api" 2>/dev/null || true)"
  elif command -v wget &>/dev/null; then
    json="$(wget -qO- "$api" 2>/dev/null || true)"
  else
    err "Neither curl nor wget is available. Install one and retry."
  fi

  DOWNLOAD_URL="$(printf '%s' "$json" \
    | grep '"browser_download_url"' \
    | grep "$BINARY\"" \
    | head -1 \
    | sed 's/.*"browser_download_url": *"\([^"]*\)".*/\1/')"

  if [[ -z "$DOWNLOAD_URL" ]]; then
    warn "No pre-built release found. Building from source (~5-10 min)…"
    BUILD_FROM_SOURCE=1
  else
    log "Found release: $BINARY"
    BUILD_FROM_SOURCE=0
  fi
}

# ── 3a. Download and install pre-built binary ─────────────
install_binary() {
  mkdir -p "$INSTALL_DIR"
  info "Downloading to $INSTALL_DIR/omitfs …"

  if command -v curl &>/dev/null; then
    curl -sSfL "$DOWNLOAD_URL" -o "$INSTALL_DIR/omitfs"
  else
    wget -qO "$INSTALL_DIR/omitfs" "$DOWNLOAD_URL"
  fi

  chmod +x "$INSTALL_DIR/omitfs"
  log "Installed → $INSTALL_DIR/omitfs"
}

# ── 3b. Fallback: build from source ───────────────────────
build_from_source() {
  # Install Rust if missing
  if ! command -v cargo &>/dev/null; then
    info "Rust not found — installing via rustup…"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --quiet --no-modify-path
    # shellcheck source=/dev/null
    source "$HOME/.cargo/env"
    log "Rust $(rustc --version)"
  fi

  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT

  info "Cloning source code…"
  git clone --depth 1 "https://github.com/${REPO}.git" "$tmp" 2>/dev/null

  info "Compiling (release build)…"
  cargo build --release --manifest-path "$tmp/Cargo.toml"

  mkdir -p "$INSTALL_DIR"
  cp "$tmp/target/release/omitfs" "$INSTALL_DIR/omitfs"
  chmod +x "$INSTALL_DIR/omitfs"
  log "Compiled and installed → $INSTALL_DIR/omitfs"
}

# ── 4. Add install dir to PATH ────────────────────────────
add_to_path() {
  if echo ":$PATH:" | grep -q ":${INSTALL_DIR}:"; then
    log "PATH already contains $INSTALL_DIR"
    return
  fi

  local shell_rc=""
  case "${SHELL:-}" in
    *zsh*)  shell_rc="$HOME/.zshrc" ;;
    *fish*) 
      info "Fish shell detected. Adding path via fish_add_path…"
      fish -c "fish_add_path $INSTALL_DIR" 2>/dev/null || \
        warn "Run manually: fish_add_path $INSTALL_DIR"
      return ;;
    *)      shell_rc="$HOME/.bashrc" ;;
  esac

  printf '\n# OmitFS\nexport PATH="$PATH:%s"\n' "$INSTALL_DIR" >> "$shell_rc"
  export PATH="$PATH:$INSTALL_DIR"
  log "Added to PATH in $shell_rc (restart shell or: source $shell_rc)"
}

# ── 5. Check / install Ollama ─────────────────────────────
check_ollama() {
  if ! command -v ollama &>/dev/null; then
    warn "Ollama not found — RAG 'ask' mode won't work without it."
    warn "Install: https://ollama.com  then: ollama pull llama3"
    return
  fi
  log "Ollama found: $(ollama --version 2>/dev/null | head -1)"
  if ollama list 2>/dev/null | grep -q "llama3"; then
    log "llama3 model already available"
  else
    info "Pulling llama3 model (this takes a few minutes)…"
    ollama pull llama3 || warn "Could not pull llama3. Run: ollama pull llama3"
  fi
}

# ── 6. Init omitfs (download embedding weights) ───────────
run_init() {
  info "Running omitfs init (downloads ~80 MB model weights once)…"
  "$INSTALL_DIR/omitfs" init || err "omitfs init failed. See output above."
}

# ── Main ──────────────────────────────────────────────────
main() {
  clear
  banner

  step "1/5" "Detecting platform"
  detect_platform

  step "2/5" "Installing binary"
  fetch_release_url
  if [[ "${BUILD_FROM_SOURCE:-0}" == "1" ]]; then
    build_from_source
  else
    install_binary
  fi

  step "3/5" "Configuring PATH"
  add_to_path

  step "4/5" "Checking Ollama (RAG / Ask AI)"
  check_ollama

  step "5/5" "Initializing OmitFS"
  run_init

  printf "\n${G}${BOLD}╔══════════════════════════════════════════════╗${NC}\n"
  printf "${G}${BOLD}║   ✅  OmitFS installed successfully!         ║${NC}\n"
  printf "${G}${BOLD}╚══════════════════════════════════════════════╝${NC}\n\n"
  printf "  ${BOLD}Restart your shell, then:${NC}\n\n"
  printf "  ${Y}# Drop files in${NC}\n"
  printf "  cp myfile.pdf ~/.omitfs_data/raw/\n\n"
  printf "  ${Y}# Start daemon${NC}\n"
  printf "  omitfs daemon &\n\n"
  printf "  ${Y}# Search${NC}\n"
  printf "  omitfs select \"calculus assignment\"\n\n"
  printf "  ${Y}# Ask AI${NC}\n"
  printf "  omitfs ask \"What formula did I derive in chapter 4?\"\n\n"
  printf "  ${Y}# Web UI${NC}\n"
  printf "  omitfs serve   # → http://localhost:3030\n\n"
}

main
