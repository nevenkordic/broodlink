# Broodlink installer for Windows
# Usage:
#   Install:   irm https://raw.githubusercontent.com/nevenkordic/broodlink/main/install.ps1 | iex
#   Uninstall: & ([scriptblock]::Create((irm https://raw.githubusercontent.com/nevenkordic/broodlink/main/install.ps1))) -Uninstall
param([switch]$Uninstall)

$ErrorActionPreference = "Stop"
$Repo = "nevenkordic/broodlink"
$InstallDir = "$env:LOCALAPPDATA\Broodlink\bin"
$Bins = @("broodlink.exe", "beads-bridge.exe", "coordinator.exe", "heartbeat.exe",
          "embedding-worker.exe", "status-api.exe", "mcp-server.exe", "a2a-gateway.exe")

# --- Uninstall ----------------------------------------------------------------
if ($Uninstall) {
    Write-Host ""
    Write-Host "  Uninstalling Broodlink..." -ForegroundColor Yellow
    $Removed = 0
    foreach ($bin in $Bins) {
        $p = Join-Path $InstallDir $bin
        if (Test-Path $p) { Remove-Item $p -Force; $Removed++ }
    }
    # Remove install dir if empty
    if ((Test-Path $InstallDir) -and @(Get-ChildItem $InstallDir).Count -eq 0) {
        Remove-Item $InstallDir -Force
    }
    # Remove from PATH
    $UserPath = [Environment]::GetEnvironmentVariable("PATH", "User")
    if ($UserPath -like "*$InstallDir*") {
        $NewPath = ($UserPath.Split(";") | Where-Object { $_ -ne $InstallDir }) -join ";"
        [Environment]::SetEnvironmentVariable("PATH", $NewPath, "User")
        Write-Host "  Removed $InstallDir from PATH."
    }
    Write-Host "  Removed $Removed binaries."
    Write-Host "  Broodlink uninstalled." -ForegroundColor Green
    Write-Host ""
    return
}

# --- Install ------------------------------------------------------------------
$Target = "x86_64-pc-windows-msvc"

Write-Host ""
Write-Host "  Broodlink Installer" -ForegroundColor Cyan
Write-Host "  ===================" -ForegroundColor DarkCyan
Write-Host ""

# Step 1: Fetch release info
Write-Host "  [1/4] Fetching latest release... " -NoNewline
$Release = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest"
$Tag = $Release.tag_name
Write-Host "$Tag" -ForegroundColor Green

$Archive = "broodlink-$Tag-$Target.zip"
$Url = "https://github.com/$Repo/releases/download/$Tag/$Archive"
$ShaUrl = "$Url.sha256"

Write-Host "         Platform: $Target"
Write-Host ""

$Tmp = New-Item -ItemType Directory -Path (Join-Path $env:TEMP "broodlink-install-$(Get-Random)")
try {
    # Step 2: Download
    Write-Host "  [2/4] Downloading ($([math]::Round(45/1, 0)) MB)..."
    $ProgressPreference = "Continue"
    Invoke-WebRequest -Uri $Url -OutFile "$Tmp\$Archive"
    Write-Host "         done" -ForegroundColor Green

    # Step 3: Verify checksum
    Write-Host "  [3/4] Verifying checksum... " -NoNewline
    Invoke-WebRequest -Uri $ShaUrl -OutFile "$Tmp\$Archive.sha256"
    $Expected = (Get-Content "$Tmp\$Archive.sha256" -Raw).Trim().Split(" ")[0]
    $Actual = (Get-FileHash "$Tmp\$Archive" -Algorithm SHA256).Hash.ToLower()
    if ($Expected -ne $Actual) {
        Write-Host "FAILED" -ForegroundColor Red
        throw "Checksum mismatch! Expected $Expected, got $Actual"
    }
    Write-Host "OK" -ForegroundColor Green

    # Step 4: Extract and install
    Write-Host "  [4/4] Installing binaries... " -NoNewline
    $ExtractDir = Join-Path $Tmp "extracted"
    Expand-Archive "$Tmp\$Archive" -DestinationPath $ExtractDir -Force

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    $Installed = 0
    foreach ($bin in $Bins) {
        $BinPath = Get-ChildItem -Path $ExtractDir -Filter $bin -Recurse | Select-Object -First 1
        if ($BinPath) {
            Copy-Item $BinPath.FullName $InstallDir -Force
            $Installed++
        } else {
            Write-Warning "Binary $bin not found in archive"
        }
    }

    # Add to PATH if not already there
    $UserPath = [Environment]::GetEnvironmentVariable("PATH", "User")
    if ($UserPath -notlike "*$InstallDir*") {
        [Environment]::SetEnvironmentVariable("PATH", "$UserPath;$InstallDir", "User")
        $env:PATH = "$env:PATH;$InstallDir"
    }
    Write-Host "done ($Installed binaries)" -ForegroundColor Green

    Write-Host ""
    Write-Host "  Broodlink $Tag installed successfully!" -ForegroundColor Green
    Write-Host "  Location: $InstallDir" -ForegroundColor DarkGray
    Write-Host ""
    Write-Host "  Get started:"
    Write-Host "    broodlink          Launch setup wizard + all services"
    Write-Host "    broodlink --help   Show all commands"
    Write-Host ""
    Write-Host "  To uninstall:"
    Write-Host "    & ([scriptblock]::Create((irm https://raw.githubusercontent.com/nevenkordic/broodlink/main/install.ps1))) -Uninstall"
    Write-Host ""
}
finally {
    Remove-Item $Tmp -Recurse -Force -ErrorAction SilentlyContinue
}
