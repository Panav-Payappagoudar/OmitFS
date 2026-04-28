# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
#  OmitFS — One-Line Installer (Windows PowerShell)
#
#  Usage (run in PowerShell as Administrator or normal user):
#    irm https://raw.githubusercontent.com/Panav-Payappagoudar/OmitFS/main/install.ps1 | iex
#
#  What this does:
#    1. Detects your Windows architecture (x86_64)
#    2. Downloads the pre-built .exe from GitHub Releases
#    3. Installs it to %USERPROFILE%\.local\bin\omitfs.exe
#    4. Adds that folder to your user PATH permanently
#    5. Checks for Ollama (optional, for "ask" / RAG mode)
#    6. Runs `omitfs init` to download embedding model weights (~80 MB, once)
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
#Requires -Version 5.1
$ErrorActionPreference = "Stop"

# ── Config ────────────────────────────────────────────────
$REPO        = "Panav-Payappagoudar/OmitFS"
$BINARY      = "omitfs-windows-x86_64.exe"
$INSTALL_DIR = "$env:USERPROFILE\.local\bin"
$INSTALL_PATH= Join-Path $INSTALL_DIR "omitfs.exe"

# ── Colors ────────────────────────────────────────────────
function Write-Banner {
    $c = @{ForegroundColor = "Magenta"}
    Write-Host ""
    Write-Host "  ╔══════════════════════════════════════════╗" @c
    Write-Host "  ║          ██████╗ ███╗   ███╗██╗████████╗ ║" @c
    Write-Host "  ║         ██╔═══██╗████╗ ████║██║╚══██╔══╝ ║" @c
    Write-Host "  ║         ██║   ██║██╔████╔██║██║   ██║    ║" @c
    Write-Host "  ║         ██║   ██║██║╚██╔╝██║██║   ██║    ║" @c
    Write-Host "  ║         ╚██████╔╝██║ ╚═╝ ██║██║   ██║    ║" @c
    Write-Host "  ║          ╚═════╝ ╚═╝     ╚═╝╚═╝   ╚═╝    ║" @c
    Write-Host "  ║  Intent-Driven Local Semantic File System  ║" @c
    Write-Host "  ╚══════════════════════════════════════════╝" @c
    Write-Host ""
}

function Write-Step  ($n, $msg) { Write-Host "`n[$n] $msg" -ForegroundColor Cyan -NoNewline; Write-Host "" }
function Write-Ok    ($msg)     { Write-Host "  ✓  $msg"  -ForegroundColor Green }
function Write-Info  ($msg)     { Write-Host "  →  $msg"  -ForegroundColor Cyan }
function Write-Warn  ($msg)     { Write-Host "  ⚠  $msg"  -ForegroundColor Yellow }
function Write-Fail  ($msg)     { Write-Host "  ✗  $msg"  -ForegroundColor Red; exit 1 }

# ── Step 1: Detect architecture ───────────────────────────
function Step-DetectPlatform {
    Write-Step "1/5" "Detecting platform"
    $arch = if ([Environment]::Is64BitOperatingSystem) { "x86_64" } else { Write-Fail "32-bit Windows is not supported." }
    Write-Ok "Windows / $arch"
}

# ── Step 2: Download binary from GitHub Releases ──────────
function Step-Install {
    Write-Step "2/5" "Downloading OmitFS binary"

    # Create install directory
    if (-not (Test-Path $INSTALL_DIR)) {
        New-Item -ItemType Directory -Path $INSTALL_DIR -Force | Out-Null
    }

    # Query GitHub Releases API
    $apiUrl = "https://api.github.com/repos/$REPO/releases/latest"
    try {
        $release    = Invoke-RestMethod -Uri $apiUrl -UseBasicParsing
        $asset      = $release.assets | Where-Object { $_.name -eq $BINARY } | Select-Object -First 1
        $downloadUrl = $asset.browser_download_url
    } catch {
        $downloadUrl = $null
    }

    if ($downloadUrl) {
        Write-Info "Downloading $BINARY …"
        Invoke-WebRequest -Uri $downloadUrl -OutFile $INSTALL_PATH -UseBasicParsing
        Write-Ok "Installed → $INSTALL_PATH"
    } else {
        # Fallback: build from source
        Write-Warn "No pre-built release found. Building from source requires Rust."
        Step-BuildFromSource
    }
}

# ── Fallback: build from source ───────────────────────────
function Step-BuildFromSource {
    # Check for Rust
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Write-Info "Installing Rust via rustup-init.exe …"
        $rustupUrl = "https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe"
        $tmpRustup  = "$env:TEMP\rustup-init.exe"
        Invoke-WebRequest -Uri $rustupUrl -OutFile $tmpRustup -UseBasicParsing
        Start-Process -FilePath $tmpRustup -ArgumentList "-y","--quiet" -Wait
        # Refresh PATH to pick up cargo
        $env:PATH = [System.Environment]::GetEnvironmentVariable("Path","Machine") + ";" +
                    [System.Environment]::GetEnvironmentVariable("Path","User")
        Write-Ok "Rust installed"
    }

    # Clone and build
    $tmpDir = Join-Path $env:TEMP "omitfs_build"
    if (Test-Path $tmpDir) { Remove-Item $tmpDir -Recurse -Force }
    Write-Info "Cloning source code …"
    git clone --depth 1 "https://github.com/$REPO.git" $tmpDir 2>$null
    Write-Info "Building release binary (this takes ~5-10 min) …"
    Push-Location $tmpDir
    cargo build --release
    Pop-Location
    Copy-Item "$tmpDir\target\release\omitfs.exe" $INSTALL_PATH
    Remove-Item $tmpDir -Recurse -Force
    Write-Ok "Built and installed → $INSTALL_PATH"
}

# ── Step 3: Add to user PATH ──────────────────────────────
function Step-AddToPath {
    Write-Step "3/5" "Configuring PATH"

    $currentPath = [Environment]::GetEnvironmentVariable("PATH", "User")
    if ($currentPath -notlike "*$INSTALL_DIR*") {
        $newPath = "$currentPath;$INSTALL_DIR"
        [Environment]::SetEnvironmentVariable("PATH", $newPath, "User")
        $env:PATH += ";$INSTALL_DIR"
        Write-Ok "Added $INSTALL_DIR to user PATH (permanent)"
    } else {
        Write-Ok "PATH already contains $INSTALL_DIR"
    }
}

# ── Step 4: Check Ollama ──────────────────────────────────
function Step-CheckOllama {
    Write-Step "4/5" "Checking Ollama (RAG / Ask AI — optional)"

    if (Get-Command ollama -ErrorAction SilentlyContinue) {
        Write-Ok "Ollama is installed"
        $models = ollama list 2>$null
        if ($models -notmatch "llama3") {
            Write-Info "Pulling llama3 model (this may take a few minutes) …"
            ollama pull llama3
        } else {
            Write-Ok "llama3 model already available"
        }
    } else {
        Write-Warn "Ollama not found. Install from: https://ollama.com"
        Write-Warn "After installing, run: ollama pull llama3"
    }
}

# ── Step 5: Initialize OmitFS ─────────────────────────────
function Step-Init {
    Write-Step "5/5" "Initializing OmitFS"
    Write-Info "Downloading embedding model (~80 MB, one-time) …"
    & $INSTALL_PATH init
    Write-Ok "OmitFS initialized at $env:USERPROFILE\.omitfs_data"
}

# ── Main ──────────────────────────────────────────────────
Clear-Host
Write-Banner

Step-DetectPlatform
Step-Install
Step-AddToPath
Step-CheckOllama
Step-Init

Write-Host ""
Write-Host "  ╔══════════════════════════════════════════════╗" -ForegroundColor Green
Write-Host "  ║   ✅  OmitFS installed successfully!         ║" -ForegroundColor Green
Write-Host "  ╚══════════════════════════════════════════════╝" -ForegroundColor Green
Write-Host ""
Write-Host "  Restart PowerShell, then:" -ForegroundColor White
Write-Host ""
Write-Host "  # Drop files in" -ForegroundColor Yellow
Write-Host '  Copy-Item myfile.pdf "$env:USERPROFILE\.omitfs_data\raw\"' -ForegroundColor Gray
Write-Host ""
Write-Host "  # Start daemon" -ForegroundColor Yellow
Write-Host "  Start-Job { omitfs daemon }" -ForegroundColor Gray
Write-Host ""
Write-Host "  # Search" -ForegroundColor Yellow
Write-Host '  omitfs select "calculus assignment"' -ForegroundColor Gray
Write-Host ""
Write-Host "  # Ask AI" -ForegroundColor Yellow
Write-Host '  omitfs ask "What formula did I derive in chapter 4?"' -ForegroundColor Gray
Write-Host ""
Write-Host "  # Web UI" -ForegroundColor Yellow
Write-Host "  omitfs serve   # → http://localhost:3030" -ForegroundColor Gray
Write-Host ""
