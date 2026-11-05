#Requires -Version 5.1
<#
.SYNOPSIS
    Local release script for Lexena. Builds, stages artifacts, creates GitHub release.

.PARAMETER Prerelease
    Mark the GitHub release as a prerelease.

.PARAMETER DryRun
    Run all steps (including build) but skip git tag and GitHub release creation.

.EXAMPLE
    .\scripts\release.ps1
    .\scripts\release.ps1 -Prerelease
    .\scripts\release.ps1 -DryRun
#>
[CmdletBinding()]
param(
    [switch]$Prerelease,
    [switch]$DryRun,
    [ValidateSet("x86_64-pc-windows-msvc")][string]$Target = "x86_64-pc-windows-msvc"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
Set-Location "$PSScriptRoot\.."
# Sync .NET current directory with PowerShell's $PWD so [System.IO.File] resolves
# relative paths against the project root, not the shell's launch dir.
[System.Environment]::CurrentDirectory = (Get-Location).Path

function Write-Step { param([string]$msg) Write-Host "" ; Write-Host ">> $msg" -ForegroundColor Cyan }
function Write-Ok   { param([string]$msg) Write-Host "   OK  $msg" -ForegroundColor Green }
function Write-Warn { param([string]$msg) Write-Host "   WARN $msg" -ForegroundColor Yellow }
function Write-Fail { param([string]$msg) Write-Host "   ERR $msg" -ForegroundColor Red; exit 1 }

function Resolve-Glob {
    param([string]$Pattern)
    return Get-ChildItem $Pattern -ErrorAction SilentlyContinue | Select-Object -First 1
}

# --- Step 1: Read version ---

Write-Step "Reading version from tauri.conf.json"

$confPath = "src-tauri\tauri.conf.json"
if (-not (Test-Path $confPath)) { Write-Fail "Cannot find $confPath" }

$conf = Get-Content $confPath -Raw | ConvertFrom-Json
$VERSION = $conf.version
$TAG = "v$VERSION"

Write-Ok "Version : $VERSION  ->  Tag : $TAG"

# --- Step 2: Prerequisites ---

Write-Step "Checking prerequisites"

$privateKey = "src-tauri\private.key"
if (-not (Test-Path $privateKey)) {
    Write-Fail "Private key not found at '$privateKey'. Run: pnpm tauri signer generate --write-keys src-tauri/private.key --ci -p `"`""
}
Write-Ok "Private key present"

try {
    $null = gh auth status 2>&1
    Write-Ok "gh CLI authenticated"
} catch {
    Write-Fail "gh CLI not authenticated. Run: gh auth login"
}

# --- Step 3: Git state ---

Write-Step "Checking git state"

$gitStatus = git status --porcelain 2>&1
if ($gitStatus) {
    Write-Warn "Working tree is dirty. Uncommitted changes will NOT be included in the release."
    Write-Warn "Files: $($gitStatus -join ', ')"
}

foreach ($t in @($TAG, $VERSION, "v-$VERSION")) {
    $existingTag = git tag -l $t 2>&1
    if ($existingTag -eq $t) {
        Write-Fail "Tag '$t' already exists locally. Bump the version first."
    }
    $remoteTag = git ls-remote --tags origin "refs/tags/$t" 2>&1
    if ($remoteTag) {
        Write-Fail "Tag '$t' already exists on remote. Bump the version first."
    }
}

Write-Ok "Tag '$TAG' is free"

# Ask Cargo for the effective target directory, including environment/config overrides.
# Use an explicit target for both Tauri and its sidecar hook; never infer one from filenames.
Push-Location "src-tauri"
try {
    $metadataText = cargo metadata --locked --no-deps --format-version 1
    if ($LASTEXITCODE -ne 0) { throw "Could not resolve Cargo target directory" }
} finally { Pop-Location }
$targetRoot = [IO.Path]::GetFullPath(($metadataText | ConvertFrom-Json).target_directory)
$releaseDir = Join-Path $targetRoot "$Target\release"
$bundleDir = [IO.Path]::GetFullPath((Join-Path $releaseDir "bundle"))
if (-not $bundleDir.StartsWith($targetRoot.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase) -or (Split-Path $bundleDir -Leaf) -ne "bundle") {
    Write-Fail "Refusing unsafe bundle cleanup path: $bundleDir"
}

# --- Step 4: Build ---

Write-Step "Building application (pnpm tauri build)"

# Ensure native build dependencies are available (LLVM/bindgen, cmake, Vulkan SDK)
if (-not $env:LIBCLANG_PATH) { $env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin" }
if ($env:PATH -notlike "*CMake*") { $env:PATH = $env:PATH + ";C:\Program Files\CMake\bin" }
if (-not $env:VULKAN_SDK) {
    $vulkanSdkReg = (Get-ItemProperty -Path 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment' -Name VULKAN_SDK -ErrorAction SilentlyContinue).VULKAN_SDK
    if ($vulkanSdkReg) {
        $env:VULKAN_SDK = $vulkanSdkReg
        Write-Ok "VULKAN_SDK set from registry: $env:VULKAN_SDK"
    } else {
        $sdkDir = Get-ChildItem "C:\VulkanSDK" -Directory -ErrorAction SilentlyContinue | Sort-Object Name -Descending | Select-Object -First 1
        if ($sdkDir) {
            $env:VULKAN_SDK = $sdkDir.FullName
            Write-Ok "VULKAN_SDK auto-detected: $env:VULKAN_SDK"
        } else {
            Write-Fail "VULKAN_SDK not found. Install the Vulkan SDK from https://vulkan.lunarg.com and reopen your terminal."
        }
    }
}

$privateKeyContent = Get-Content $privateKey -Raw
$env:TAURI_SIGNING_PRIVATE_KEY = $privateKeyContent.Trim()
# $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = ""

Write-Ok "Cleaning old bundle artifacts to avoid stale files"
if (Test-Path -LiteralPath $bundleDir) { Remove-Item -LiteralPath $bundleDir -Force -Recurse }

try {
    pnpm tauri build --target $Target
    if ($LASTEXITCODE -ne 0) { throw "Build failed (exit code $LASTEXITCODE)" }
} finally {
    Remove-Item Env:\TAURI_SIGNING_PRIVATE_KEY -ErrorAction SilentlyContinue
    Remove-Item Env:\TAURI_SIGNING_PRIVATE_KEY_PASSWORD -ErrorAction SilentlyContinue
}

Write-Ok "Build succeeded"

# --- Step 5: Locate artifacts ---

Write-Step "Locating build artifacts"

$portableSrc = Join-Path $releaseDir "lexena.exe"
if (-not (Test-Path $portableSrc)) { Write-Fail "Portable EXE not found: $portableSrc" }
Write-Ok "Portable   : $portableSrc"
$bridgeSrc = Join-Path $releaseDir "lexena-mcp.exe"
$stagedBridge = "src-tauri\binaries\lexena-mcp-$Target.exe"
if (-not (Test-Path -LiteralPath $bridgeSrc)) { Write-Fail "MCP bridge not found beside application: $bridgeSrc" }
if ((Get-FileHash -LiteralPath $bridgeSrc -Algorithm SHA256).Hash -ne (Get-FileHash -LiteralPath $stagedBridge -Algorithm SHA256).Hash) {
    Write-Fail "Bundled MCP bridge differs from current target build"
}

$nsisExeSrc = Resolve-Glob "$bundleDir\nsis\Lexena_${VERSION}_x64-setup.exe"
if (-not $nsisExeSrc) { Write-Fail "NSIS setup EXE not found for version $VERSION in $bundleDir\nsis\" }
Write-Ok "NSIS EXE   : $($nsisExeSrc.FullName)"

$nsisSigPath = "$($nsisExeSrc.FullName).sig"
if (-not (Test-Path $nsisSigPath)) { Write-Fail "NSIS updater SIG not found: $nsisSigPath" }
Write-Ok "NSIS SIG   : $nsisSigPath"

# --- Step 6: Stage artifacts ---

Write-Step "Staging artifacts into artifacts\$TAG\"

$stagingDir = "artifacts\$TAG"
New-Item -ItemType Directory -Force -Path $stagingDir | Out-Null

$portableDest = "$stagingDir\lexena-$TAG-portable.exe"
$nsisExeDest  = "$stagingDir\lexena-$TAG-setup.exe"
$portableMcpDest = "$stagingDir\lexena-$TAG-portable-mcp.zip"

Copy-Item $portableSrc          $portableDest
Copy-Item $nsisExeSrc.FullName  $nsisExeDest
& "$PSScriptRoot\package-mcp-portable.ps1" -AppPath $portableSrc -BridgePath $bridgeSrc -Destination $portableMcpDest -Target $Target
if (-not (Test-Path -LiteralPath $portableMcpDest)) { Write-Fail "Portable MCP ZIP was not produced" }

Write-Ok "Staged $((Get-ChildItem $stagingDir).Count) files"

# --- Step 7: Build latest.json and latest-beta.json ---

Write-Step "Building latest.json and latest-beta.json"

$signature   = Get-Content $nsisSigPath -Raw
$nsisExeName = "lexena-$TAG-setup.exe"
$downloadUrl = "https://github.com/Nolyo/lexena/releases/download/$TAG/$nsisExeName"
$pubDate     = (Get-Date -Format "yyyy-MM-ddTHH:mm:ssZ")

$latestJson = [ordered]@{
    version   = $VERSION
    notes     = "See https://github.com/Nolyo/lexena/blob/$TAG/CHANGELOG.md"
    pub_date  = $pubDate
    platforms = [ordered]@{
        "windows-x86_64" = [ordered]@{
            signature = $signature.Trim()
            url       = $downloadUrl
        }
    }
}

$latestJsonPath     = "$stagingDir\latest.json"
$latestBetaJsonPath = "$stagingDir\latest-beta.json"
$jsonContent = $latestJson | ConvertTo-Json -Depth 5
[System.IO.File]::WriteAllText($latestJsonPath,     $jsonContent, [System.Text.UTF8Encoding]::new($false))
[System.IO.File]::WriteAllText($latestBetaJsonPath, $jsonContent, [System.Text.UTF8Encoding]::new($false))
Write-Ok "latest.json      -> $latestJsonPath"
Write-Ok "latest-beta.json -> $latestBetaJsonPath"
Write-Ok "Download URL: $downloadUrl"

# --- Step 8: SHA256 checksums ---

Write-Step "Generating SHA256 checksums"

$checksumPath = "$stagingDir\checksums-$TAG.txt"
Remove-Item $checksumPath -ErrorAction SilentlyContinue

$filesToChecksum = @($portableDest, $nsisExeDest, $portableMcpDest)
foreach ($file in $filesToChecksum) {
    $hash = (Get-FileHash $file -Algorithm SHA256).Hash.ToLower()
    $name = Split-Path $file -Leaf
    Add-Content $checksumPath "$hash *$name"
    Write-Ok "$name : $hash"
}

# --- DryRun exit ---

if ($DryRun) {
    Write-Host ""
    Write-Host "[DRY RUN] Skipping git tag, GitHub release, and releases.json update." -ForegroundColor Yellow
    Write-Host "Artifacts staged in: $stagingDir" -ForegroundColor Yellow
    exit 0
}

# --- Step 9: Git tag ---

Write-Step "Creating and pushing git tag $TAG"

git tag $TAG
if ($LASTEXITCODE -ne 0) { Write-Fail "Failed to create tag $TAG" }

git push origin $TAG
if ($LASTEXITCODE -ne 0) { Write-Fail "Failed to push tag $TAG" }

Write-Ok "Tag $TAG pushed"

# --- Step 10: GitHub Release ---

Write-Step "Creating GitHub Release $TAG"

$releaseNotes = @"
See the [CHANGELOG](https://github.com/Nolyo/lexena/blob/$TAG/CHANGELOG.md) for details.

## Downloads

- **lexena-$TAG-portable.exe** : Historical standalone portable executable (MCP requires the ZIP below)
- **lexena-$TAG-portable-mcp.zip** : Portable MCP package; extract both executables and mcp-version.json together
- **lexena-$TAG-setup.exe** : NSIS installer (recommended)

Verify file integrity using the provided SHA256 checksums.

## Auto-Update Support

This release includes signed update artifacts for automatic in-app updates.
"@

if ($Prerelease) {
    $releaseNotes += "`n`n[PRERELEASE] This is a pre-release version for testing purposes only."
}

$ghArgs = @(
    "release", "create", $TAG,
    "--title", "Lexena $TAG",
    "--notes", $releaseNotes,
    $portableDest,
    $portableMcpDest,
    $nsisExeDest,
    $latestJsonPath,
    $latestBetaJsonPath,
    $checksumPath
)

if ($Prerelease) { $ghArgs += "--prerelease" }

gh @ghArgs
if ($LASTEXITCODE -ne 0) { Write-Fail "gh release create failed" }

Write-Ok "GitHub Release created: https://github.com/Nolyo/lexena/releases/tag/$TAG"

# --- Step 10b: For prereleases, also push latest-beta.json to the latest stable release ---
# The beta updater URL resolves to 'releases/latest/download/latest-beta.json'.
# GitHub's 'latest' always points at the most recent non-prerelease, so the file
# must live there to be discoverable by users on the beta channel.

if ($Prerelease) {
    Write-Step "Updating latest-beta.json on latest stable release"

    $releases = gh release list --json tagName --limit 100 | ConvertFrom-Json
    $latestStableTag = $null
    foreach ($r in $releases) {
        if ($r.tagName -notmatch 'beta|alpha|rc|test') {
            $latestStableTag = $r.tagName
            break
        }
    }
    $stableRelease = if ($latestStableTag) { @{ tagName = $latestStableTag } } else { $null }

    if ($stableRelease) {
        $latestStableTag = $stableRelease.tagName
        Write-Ok "Target stable release: $latestStableTag"
        Write-Ok "Uploading: $latestBetaJsonPath"
        gh release upload $latestStableTag $latestBetaJsonPath --clobber
        if ($LASTEXITCODE -ne 0) {
            Write-Fail "Could not update latest-beta.json on $latestStableTag. Run manually: gh release upload $latestStableTag $latestBetaJsonPath --clobber"
        } else {
            Write-Ok "latest-beta.json pushed to stable release $latestStableTag"
            Write-Ok "Beta channel users will now be offered $TAG"
        }
    } else {
        Write-Fail "No stable release found on GitHub. Create a stable release first, then run: gh release upload <tag> $latestBetaJsonPath --clobber"
    }
}

# --- Step 11: Regenerate releases.json ---

Write-Step "Regenerating docs/releases.json"

$scriptsDir = ".github\scripts"
if (-not (Test-Path "$scriptsDir\generate-releases-json.js")) {
    Write-Warn "generate-releases-json.js not found in $scriptsDir -- skipping releases.json update"
} else {
    $env:GITHUB_TOKEN = (gh auth token)

    Push-Location $scriptsDir
    try {
        node generate-releases-json.js
        if ($LASTEXITCODE -ne 0) { Write-Warn "generate-releases-json.js exited with code $LASTEXITCODE" }
    } finally {
        Pop-Location
        Remove-Item Env:\GITHUB_TOKEN -ErrorAction SilentlyContinue
    }

    # --- Step 12: Commit and push releases.json ---

    Write-Step "Committing docs/releases.json"

    git add docs/releases.json
    $diff = git diff --staged --name-only
    if ($diff) {
        git commit -m "chore: update releases.json for $TAG"
        git push origin main
        if ($LASTEXITCODE -ne 0) { Write-Warn "Failed to push releases.json -- push manually if needed" }
        Write-Ok "docs/releases.json committed and pushed"
    } else {
        Write-Ok "docs/releases.json unchanged -- nothing to commit"
    }
}

# --- Done ---

Write-Host ""
Write-Host "[DONE] Release $TAG complete!" -ForegroundColor Green
Write-Host "       https://github.com/Nolyo/lexena/releases/tag/$TAG" -ForegroundColor Green
