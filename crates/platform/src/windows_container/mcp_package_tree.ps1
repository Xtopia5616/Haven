function Assert-HavenMcpPackageDataStreams([string]$Path) {
    if (Get-Item -LiteralPath $Path -Stream * -ErrorAction Stop | Where-Object Stream -ne ':$DATA') {
        throw 'Package contains an alternate data stream.'
    }
}

function Get-HavenMcpPackageTreeDigest {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string]$PackageRoot,

        [Parameter(Mandatory = $true)]
        [string]$Entrypoint
    )

    $maxEntries = 8192
    $maxFiles = 4096
    $maxRelativePathUnits = 1024
    $maxPackageBytes = 128MB

    $root = [System.IO.Path]::GetFullPath($PackageRoot).TrimEnd('\') + '\'
    Assert-HavenMcpPackageDataStreams -Path $root.TrimEnd('\')
    $rootAttributes = [System.IO.File]::GetAttributes($root)
    if (($rootAttributes -band [System.IO.FileAttributes]::Directory) -eq 0 -or
        ($rootAttributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw 'Package root must be a regular directory.'
    }

    $pending = [System.Collections.Generic.Stack[string]]::new()
    $pending.Push($root.TrimEnd('\'))
    $records = [System.Collections.Generic.List[string]]::new()
    $seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    $entries = 0
    $files = 0
    $totalBytes = 0L
    $entrypointFound = $false

    while ($pending.Count -gt 0) {
        $directory = $pending.Pop()
        foreach ($path in [System.IO.Directory]::EnumerateFileSystemEntries($directory)) {
            $entries += 1
            if ($entries -gt $maxEntries) {
                throw 'Package contains too many entries.'
            }

            Assert-HavenMcpPackageDataStreams -Path $path

            $attributes = [System.IO.File]::GetAttributes($path)
            if (($attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw 'Package contains a reparse point.'
            }

            $fullPath = [System.IO.Path]::GetFullPath($path)
            if (-not $fullPath.StartsWith($root, [System.StringComparison]::OrdinalIgnoreCase)) {
                throw 'Package entry escapes its root.'
            }

            if (($attributes -band [System.IO.FileAttributes]::Directory) -ne 0) {
                $pending.Push($fullPath)
                continue
            }

            $relativePath = $fullPath.Substring($root.Length).Replace('\', '/')
            if ($relativePath.Length -eq 0 -or $relativePath.Length -gt $maxRelativePathUnits) {
                throw 'Package contains an invalid relative path.'
            }
            foreach ($segment in $relativePath.Split('/')) {
                if ($segment.Length -eq 0 -or $segment -eq '.' -or $segment -eq '..' -or
                    $segment.EndsWith('.') -or $segment.EndsWith(' ') -or
                    [regex]::IsMatch($segment, '[\x00-\x1f\x7f<>:"\\|?*]') -or
                    [regex]::IsMatch($segment, '^(?i:CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?:\..*)?$')) {
                    throw 'Package contains an unsupported file name.'
                }
            }
            if (-not $seen.Add($relativePath)) {
                throw 'Package contains case-ambiguous paths.'
            }

            $files += 1
            if ($files -gt $maxFiles) {
                throw 'Package contains too many files.'
            }

            $file = [System.IO.FileInfo]::new($fullPath)
            if ($file.Length -lt 0 -or $file.Length -gt ($maxPackageBytes - $totalBytes)) {
                throw 'Package exceeds its byte limit.'
            }
            $totalBytes += $file.Length

            if ([string]::Equals($relativePath, $Entrypoint, [System.StringComparison]::OrdinalIgnoreCase)) {
                $entrypointFound = $true
            }

            $sha = [System.Security.Cryptography.SHA256]::Create()
            $fileStream = $null
            try {
                $fileStream = [System.IO.File]::OpenRead($fullPath)
                $fileHash = [System.BitConverter]::ToString($sha.ComputeHash($fileStream)).Replace('-', '').ToLowerInvariant()
            } finally {
                $sha.Dispose()
                if ($null -ne $fileStream) { $fileStream.Dispose() }
            }
            $records.Add([string]::Concat($relativePath, [char]0, $fileHash))
        }
    }

    if ($files -eq 0 -or -not $entrypointFound) {
        throw 'Package tree does not contain the configured entrypoint.'
    }

    $orderedRecords = $records.ToArray()
    [System.Array]::Sort($orderedRecords, [System.StringComparer]::OrdinalIgnoreCase)
    $canonical = [System.Text.StringBuilder]::new()
    foreach ($record in $orderedRecords) {
        [void]$canonical.Append($record).Append([char]10)
    }

    $utf8 = [System.Text.UTF8Encoding]::new($false, $true)
    $bytes = $utf8.GetBytes($canonical.ToString())
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        return [System.BitConverter]::ToString($sha.ComputeHash($bytes)).Replace('-', '').ToLowerInvariant()
    } finally {
        $sha.Dispose()
    }
}
