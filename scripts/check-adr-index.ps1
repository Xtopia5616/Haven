$ErrorActionPreference = 'Stop'

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$adrRoot = (Resolve-Path (Join-Path $repositoryRoot 'docs/adr')).Path
$adrFiles = @(Get-ChildItem -LiteralPath $adrRoot -File -Filter '*.md' | Where-Object Name -ne 'README.md')
$fileByNumber = @{}
$fileByName = @{}
$filenamePattern = '^(?<number>\d{4})-[a-z0-9]+(?:-[a-z0-9]+)*\.md$'

foreach ($file in $adrFiles) {
    $match = [regex]::Match($file.Name, $filenamePattern)
    if (-not $match.Success) {
        throw "ADR filename must use NNNN-lowercase-slug.md: $($file.Name)"
    }

    $number = $match.Groups['number'].Value
    if ($fileByNumber.ContainsKey($number)) {
        throw "Duplicate ADR number $number`: $($fileByNumber[$number]), $($file.Name)"
    }
    $fileByNumber[$number] = $file.Name
    $fileByName[$file.Name] = $number
}

$readmePath = Join-Path $adrRoot 'README.md'
$readme = Get-Content -LiteralPath $readmePath -Raw
$entryPattern = '(?m)^\s*-\s*\[(?<number>\d{4})[:：](?<title>[^\]]+)\]\((?<target>[^)]+\.md)\)\s*$'
$entries = @([regex]::Matches($readme, $entryPattern))
$allMarkdownLinks = @([regex]::Matches($readme, '\]\((?<target>[^)]+\.md)\)'))
if ($entries.Count -ne $allMarkdownLinks.Count) {
    throw "Every Markdown link in $readmePath must be one ADR index entry; parsed $($entries.Count) entries from $($allMarkdownLinks.Count) links."
}

$indexedTargets = @{}
$indexedNumbers = @{}
$indexOrder = [System.Collections.Generic.List[int]]::new()
foreach ($entry in $entries) {
    $number = $entry.Groups['number'].Value
    $target = $entry.Groups['target'].Value
    if (-not $fileByName.ContainsKey($target)) {
        throw "ADR index target does not exist: $target"
    }
    if ($fileByName[$target] -ne $number) {
        throw "ADR index number $number does not match target $target (expected $($fileByName[$target]))."
    }
    if ($indexedTargets.ContainsKey($target)) {
        throw "ADR index links to $target more than once."
    }
    if ($indexedNumbers.ContainsKey($number)) {
        throw "ADR index number $number appears more than once."
    }

    $indexedTargets[$target] = $true
    $indexedNumbers[$number] = $true
    $indexOrder.Add([int]$number)
}

$missing = @($adrFiles | Where-Object { -not $indexedTargets.ContainsKey($_.Name) } | ForEach-Object Name | Sort-Object)
if ($missing.Count -gt 0) {
    throw "ADR files missing from README index: $($missing -join ', ')"
}
if ($entries.Count -ne $adrFiles.Count) {
    throw "ADR index has $($entries.Count) entries for $($adrFiles.Count) ADR files."
}

$sortedOrder = @($indexOrder | Sort-Object)
for ($index = 0; $index -lt $indexOrder.Count; $index++) {
    if ($indexOrder[$index] -ne $sortedOrder[$index]) {
        throw "ADR README index is not in ascending number order at entry $($index + 1)."
    }
}

# Catch broken local links to ADR files outside the index as well, including
# sibling links inside ADRs and links from other project documentation.
$adrReferencePattern = '(?:^|/)adr/(?<target>\d{4}-[a-z0-9]+(?:-[a-z0-9]+)*\.md)$'
$siblingAdrPattern = '^\d{4}-[a-z0-9]+(?:-[a-z0-9]+)*\.md$'
$markdownFiles = @(Get-ChildItem -LiteralPath (Join-Path $repositoryRoot 'docs') -Recurse -File -Filter '*.md')
$markdownLinkPattern = '\[[^\]]+\]\((?<target>[^)]+)\)'
foreach ($markdownFile in $markdownFiles) {
    $isAdrDocument = [string]::Equals(
        $markdownFile.Directory.FullName.TrimEnd([IO.Path]::DirectorySeparatorChar),
        $adrRoot.TrimEnd([IO.Path]::DirectorySeparatorChar),
        [StringComparison]::OrdinalIgnoreCase
    )
    $source = Get-Content -LiteralPath $markdownFile.FullName -Raw
    foreach ($link in [regex]::Matches($source, $markdownLinkPattern)) {
        $target = $link.Groups['target'].Value.Split('#')[0].Split('?')[0]
        $isAdrReference = $target -match $adrReferencePattern -or ($isAdrDocument -and $target -match $siblingAdrPattern)
        if (-not $isAdrReference) {
            continue
        }

        $normalizedTarget = $target.Replace('/', [IO.Path]::DirectorySeparatorChar)
        $resolvedTarget = [IO.Path]::GetFullPath((Join-Path $markdownFile.DirectoryName $normalizedTarget))
        if (-not (Test-Path -LiteralPath $resolvedTarget -PathType Leaf)) {
            throw "Broken ADR link in $($markdownFile.FullName): $target"
        }
    }
}

Write-Host "ADR index covers $($adrFiles.Count) uniquely numbered records; local ADR links resolve."
