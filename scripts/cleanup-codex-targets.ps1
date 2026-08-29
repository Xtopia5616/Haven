[CmdletBinding(SupportsShouldProcess = $true, ConfirmImpact = 'High')]
param(
    [switch]$IncludeCurrentWorktree
)

$ErrorActionPreference = 'Stop'

$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$targetPaths = [System.Collections.Generic.List[string]]::new()

function Add-TargetPath {
    param([Parameter(Mandatory = $true)][string]$Path)

    if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
        return
    }

    $resolved = (Resolve-Path -LiteralPath $Path).Path
    if ((Split-Path -Leaf $resolved) -ne 'target') {
        throw "Refusing to remove a non-target directory: $resolved"
    }

    if (-not $targetPaths.Contains($resolved)) {
        $targetPaths.Add($resolved)
    }
}

if ($IncludeCurrentWorktree) {
    Add-TargetPath -Path (Join-Path $repoRoot 'target')
}

$userProfile = [Environment]::GetFolderPath('UserProfile')
$codexWorktreesRoot = Join-Path $userProfile '.codex\worktrees'
if (Test-Path -LiteralPath $codexWorktreesRoot -PathType Container) {
    Get-ChildItem -LiteralPath $codexWorktreesRoot -Directory -Force | ForEach-Object {
        Add-TargetPath -Path (Join-Path $_.FullName 'Haven\target')
    }
}

if ($targetPaths.Count -eq 0) {
    Write-Output 'No Codex Rust target directories found.'
    exit 0
}

$totalBytes = ($targetPaths | ForEach-Object {
    (Get-ChildItem -LiteralPath $_ -File -Recurse -Force -ErrorAction SilentlyContinue |
        Measure-Object -Property Length -Sum).Sum
} | Measure-Object -Sum).Sum

Write-Output ("Found {0} target directories ({1:N2} GB)." -f $targetPaths.Count, ($totalBytes / 1GB))
foreach ($targetPath in $targetPaths) {
    if ($PSCmdlet.ShouldProcess($targetPath, 'Remove Rust build artifacts')) {
        Remove-Item -LiteralPath $targetPath -Recurse -Force
        Write-Output "Removed $targetPath"
    }
}
