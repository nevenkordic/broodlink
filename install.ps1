# Broodlink installer for Windows
# Usage: irm https://raw.githubusercontent.com/nevenkordic/broodlink/main/install.ps1 | iex
$ErrorActionPreference = "Stop"

$Repo = "nevenkordic/broodlink"
$InstallDir = "$env:LOCALAPPDATA\Broodlink\bin"
$Target = "x86_64-pc-windows-msvc"

# Get latest release tag
Write-Host "Fetching latest Broodlink release..."
$Release = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest"
$Tag = $Release.tag_name

$Archive = "broodlink-$Tag-$Target.zip"
$Url = "https://github.com/$Repo/releases/download/$Tag/$Archive"
$ShaUrl = "$Url.sha256"

Write-Host "Installing Broodlink $Tag for Windows..."

# Download to temp
$Tmp = New-Item -ItemType Directory -Path (Join-Path $env:TEMP "broodlink-install-$(Get-Random)")
try {
    Invoke-WebRequest -Uri $Url -OutFile "$Tmp\$Archive"
    Invoke-WebRequest -Uri $ShaUrl -OutFile "$Tmp\$Archive.sha256"

    # Verify checksum
    $Expected = (Get-Content "$Tmp\$Archive.sha256" -Raw).Trim().Split(" ")[0]
    $Actual = (Get-FileHash "$Tmp\$Archive" -Algorithm SHA256).Hash.ToLower()
    if ($Expected -ne $Actual) {
        throw "Checksum mismatch! Expected $Expected, got $Actual"
    }
    Write-Host "Checksum verified."

    # Extract
    $ExtractDir = Join-Path $Tmp "extracted"
    Expand-Archive "$Tmp\$Archive" -DestinationPath $ExtractDir -Force

    # Find binaries — zip may contain files flat or inside a subdirectory
    $Dir = $ExtractDir
    $SubDir = Join-Path $ExtractDir "broodlink-$Tag-$Target"
    if (Test-Path $SubDir) { $Dir = $SubDir }

    # Install
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    $Bins = @("broodlink.exe", "beads-bridge.exe", "coordinator.exe", "heartbeat.exe",
              "embedding-worker.exe", "status-api.exe", "mcp-server.exe", "a2a-gateway.exe")
    foreach ($bin in $Bins) {
        $BinPath = Get-ChildItem -Path $Dir -Filter $bin -Recurse | Select-Object -First 1
        if ($BinPath) {
            Copy-Item $BinPath.FullName $InstallDir -Force
        } else {
            Write-Warning "Binary $bin not found in archive"
        }
    }

    # Add to PATH if not already there
    $UserPath = [Environment]::GetEnvironmentVariable("PATH", "User")
    if ($UserPath -notlike "*$InstallDir*") {
        [Environment]::SetEnvironmentVariable("PATH", "$UserPath;$InstallDir", "User")
        $env:PATH = "$env:PATH;$InstallDir"
        Write-Host "Added $InstallDir to PATH."
    }

    Write-Host ""
    Write-Host "Broodlink $Tag installed successfully!" -ForegroundColor Green
    Write-Host ""
    Write-Host "  Run 'broodlink' to launch the setup wizard and start all services."
    Write-Host ""
}
finally {
    Remove-Item $Tmp -Recurse -Force -ErrorAction SilentlyContinue
}
