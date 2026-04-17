#
# Launch (or restart) the MyLua Extension Development Host
# opening tests/lua-root as the workspace.
#
# Usage:
#   .cursor/scripts/test-extension.ps1 [-SkipBuild] [-SkipLsp] [-SkipExt] [-w] [-w 0] [-w 1]
#
param(
    [switch]$SkipBuild,
    [switch]$SkipLsp,
    [switch]$SkipExt,
    [Alias("w")]
    [switch]$UseWorkspace
)

$ErrorActionPreference = "Stop"

$RepoRoot = Resolve-Path (Join-Path $PSScriptRoot "../..")
$ExtDir   = Join-Path $RepoRoot "vscode-extension"
$LuaRoot  = Join-Path $RepoRoot "tests/lua-root"
$WorkspaceFile = Join-Path $RepoRoot "tests/mylua-tests.code-workspace"
$EdhMarker = "extensionDevelopmentPath=$ExtDir"

$OpenWorkspace = $UseWorkspace.IsPresent
if ($UseWorkspace.IsPresent) {
    if ($args.Count -gt 0) {
        if ($args.Count -gt 1 -or $args[0] -notin @("0", "1")) {
            Write-Error "Invalid -w value. Use -w, -w 0, or -w 1."
            exit 1
        }
        $OpenWorkspace = [int]$args[0] -eq 1
    }
} elseif ($args.Count -gt 0) {
    Write-Error "Unknown argument: $($args[0]). Use -w, -w 0, or -w 1."
    exit 1
}

$LaunchTarget = if ($OpenWorkspace) { $WorkspaceFile } else { $LuaRoot }
$LaunchTargetLabel = if ($OpenWorkspace) { "workspace" } else { "lua-root" }

if (-not (Test-Path $LaunchTarget)) {
    Write-Error "Launch target not found: $LaunchTarget"
    exit 1
}

# Auto-detect editor CLI: prefer code, fall back to cursor
$EditorCli = $null
if (Get-Command "code" -ErrorAction SilentlyContinue) {
    $EditorCli = "code"
} elseif (Get-Command "cursor" -ErrorAction SilentlyContinue) {
    $EditorCli = "cursor"
} else {
    Write-Error "Neither 'code' nor 'cursor' CLI found in PATH."
    exit 1
}

# ── Step 1: Build LSP server ──────────────────────────────────────────
if (-not $SkipBuild -and -not $SkipLsp) {
    Write-Host "==> [1/4] Building LSP server (cargo build)..."
    Push-Location (Join-Path $RepoRoot "lsp")
    cargo build
    if ($LASTEXITCODE -ne 0) { Pop-Location; exit $LASTEXITCODE }
    Pop-Location
} else {
    Write-Host "==> [1/4] Skipping LSP build"
}

# ── Step 2: Compile extension ─────────────────────────────────────────
if (-not $SkipBuild -and -not $SkipExt) {
    Write-Host "==> [2/4] Compiling VS Code extension (npm run compile)..."
    Push-Location $ExtDir
    npm run compile
    if ($LASTEXITCODE -ne 0) { Pop-Location; exit $LASTEXITCODE }
    Pop-Location
} else {
    Write-Host "==> [2/4] Skipping extension compile"
}

# ── Step 3: Kill existing Extension Development Host ──────────────────
Write-Host "==> [3/4] Checking for existing Extension Development Host..."
$edhProcesses = Get-WmiObject Win32_Process -ErrorAction SilentlyContinue |
    Where-Object { $_.CommandLine -and $_.CommandLine.Contains($EdhMarker) }

if ($edhProcesses) {
    $pids = ($edhProcesses | ForEach-Object { $_.ProcessId }) -join ", "
    Write-Host "    Found running EDH (PIDs: $pids). Terminating..."
    $edhProcesses | ForEach-Object {
        Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Seconds 2
    Write-Host "    Previous instance terminated."
} else {
    Write-Host "    No existing instance found."
}

# ── Step 4: Launch Extension Development Host ─────────────────────────
Write-Host "==> [4/4] Launching Extension Development Host ($EditorCli)..."
Write-Host "    Extension: $ExtDir"
Write-Host "    Target ($LaunchTargetLabel): $LaunchTarget"
Start-Process $EditorCli -ArgumentList "--extensionDevelopmentPath=`"$ExtDir`"", "`"$LaunchTarget`""

Write-Host ""
Write-Host "==> Done! Extension Development Host launched with $LaunchTargetLabel."
Write-Host "    Run again to restart."
