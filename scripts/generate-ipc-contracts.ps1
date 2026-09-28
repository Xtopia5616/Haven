param(
    [switch] $Check
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$mode = if ($Check) { '--check' } else { '--write' }

Push-Location $root
try {
    & cargo run --locked --no-default-features --features ipc-codegen -p haven-app-binary --bin generate_ipc_contracts -- $mode
    if ($LASTEXITCODE -ne 0) {
        throw "IPC contract generator failed with exit code $LASTEXITCODE"
    }
}
finally {
    Pop-Location
}
