#Requires -Version 5.1
param(
    [Parameter(Mandatory = $true)][string]$AppPath,
    [Parameter(Mandatory = $true)][string]$BridgePath,
    [Parameter(Mandatory = $true)][string]$Destination,
    [ValidateSet("x86_64-pc-windows-msvc")][string]$Target = "x86_64-pc-windows-msvc"
)
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$app = (Resolve-Path -LiteralPath $AppPath).Path
$bridge = (Resolve-Path -LiteralPath $BridgePath).Path
$zip = [IO.Path]::GetFullPath($Destination)
$temporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$temporaryName = "lexena-mcp-package-" + [guid]::NewGuid().ToString("N")
$temporary = [IO.Path]::GetFullPath((Join-Path $temporaryRoot $temporaryName))
# Validate the exact recursive cleanup target before creating it.
if ((Split-Path $temporary -Parent).TrimEnd('\') -ne $temporaryRoot.TrimEnd('\') -or (Split-Path $temporary -Leaf) -ne $temporaryName) {
    throw "Invalid portable staging directory"
}
New-Item -ItemType Directory -Path $temporary | Out-Null
try {
    Copy-Item -LiteralPath $app -Destination (Join-Path $temporary "lexena.exe")
    Copy-Item -LiteralPath $bridge -Destination (Join-Path $temporary "lexena-mcp.exe")
    node (Join-Path $PSScriptRoot "build-mcp-bridge.mjs") --portable-manifest $temporary --target $Target
    if ($LASTEXITCODE -ne 0) { throw "Portable MCP manifest validation failed" }
    node (Join-Path $PSScriptRoot "../.github/scripts/scan-secrets-in-bundles.mjs") --root $temporary
    if ($LASTEXITCODE -ne 0) { throw "Portable MCP secret scan failed" }
    $files = @("lexena.exe", "lexena-mcp.exe", "mcp-version.json") | ForEach-Object { Join-Path $temporary $_ }
    Compress-Archive -LiteralPath $files -DestinationPath $zip -Force
    Write-Output $zip
} finally {
    Remove-Item -LiteralPath $temporary -Recurse -Force
}
