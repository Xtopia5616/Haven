[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$PackageRoot,

    [Parameter(Mandatory = $true)]
    [string]$Entrypoint
)

$ErrorActionPreference = 'Stop'
$modulePath = Join-Path $PSScriptRoot '..\crates\platform\src\windows_container\mcp_package_tree.ps1'
$resolvedModulePath = (Resolve-Path -LiteralPath $modulePath).Path
. $resolvedModulePath

try {
    $digest = Get-HavenMcpPackageTreeDigest -PackageRoot $PackageRoot -Entrypoint $Entrypoint
    [Console]::Out.WriteLine($digest)
} catch {
    [Console]::Error.WriteLine('Package tree could not be hashed safely.')
    exit 1
}
