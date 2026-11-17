# Rebuilds the browser pages (web\dist) from this repository's frontend sources.
#
# update-web.bat runs this script. It is maintenance only: the normal build (cargo, build.bat)
# never runs it and needs neither Node.js nor npm, because web\dist is part of the project and
# app\build.rs embeds it into the exe as it is.
#
#   update-web.bat [-Source <frontend folder>] [-WorkDir <folder>] [-Check] [-Keep]
#                  [-UseSourceModules]
#
#   -Source <folder>     the repository's frontend folder (default ..\frontend)
#   -WorkDir <folder>    where the copy is built (default build\web-build); it is deleted and
#                        made again, and removed after success unless -Keep is given
#   -Check               only say whether web\VENDORED.json was built from the sources as they
#                        are now (exit code 0) or not (exit code 1); nothing is built
#   -Keep                keep the work folder after a successful build
#   -UseSourceModules    copy the source's node_modules into the copy instead of "npm ci"
#                        (no network needed; the copy is only read from the source)
#
# What it does, in order:
#
#   1. stops with a clear message when node or npm is not on PATH;
#   2. checks the source (package.json, index.html, src\) and computes source_sha256: SHA-256
#      over the lines "<relative path>NUL<SHA-256 of the file>LF" of every file of the source,
#      sorted by path, except node_modules\, dist\ and *.tsbuildinfo;
#   3. copies the source (without node_modules and dist) into <WorkDir>\frontend and runs
#      "npm ci" there (or copies node_modules with -UseSourceModules); nothing is ever written
#      into the source folder;
#   4. runs "npm run build -- --outDir ..\dist --emptyOutDir" in the copy (vite build);
#   5. checks the output: index.html, every /assets/... file it references, licenses\README.txt
#      and every licence file that README.txt names, and nothing outside assets\ and licenses\;
#   6. writes web\VENDORED.json and replaces web\dist (the old folder is renamed first, then
#      removed), and prints the files that changed.
#
# The file is plain ASCII on purpose (see setup.ps1).

param(
    [string]$Source = '',
    [string]$WorkDir = '',
    [switch]$Check,
    [switch]$Keep,
    [switch]$UseSourceModules
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

$ProjectRoot = $PSScriptRoot
$WebRoot = Join-Path $ProjectRoot 'web'
$DistFolder = Join-Path $WebRoot 'dist'
$VendoredFile = Join-Path $WebRoot 'VENDORED.json'
if ($Source -eq '') { $Source = Join-Path (Split-Path $ProjectRoot -Parent) 'frontend' }
if ($WorkDir -eq '') { $WorkDir = Join-Path $ProjectRoot 'build\web-build' }
# The folders and files of the source that are not part of it: installed packages, an earlier
# build of the clone itself and TypeScript's incremental build state.
$SkippedFolders = @('node_modules', 'dist')
$SkippedPattern = '*.tsbuildinfo'

function Write-Step([string]$Text) { Write-Host "[update-web] $Text" }
function Stop-Update([string]$Text) {
    Write-Host "[update-web] ERROR: $Text" -ForegroundColor Red
    exit 1
}

# The SHA-256 of a file as lower-case hexadecimal.
function Get-FileSha256([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

# The SHA-256 of a text (UTF-8) as lower-case hexadecimal.
function Get-TextSha256([string]$Text) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $bytes = $sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($Text))
    } finally {
        $sha.Dispose()
    }
    return (($bytes | ForEach-Object { $_.ToString('x2') }) -join '')
}

# The files of a folder with their paths relative to it (forward slashes), sorted ordinally.
function Get-RelativeFiles([string]$Root, [string[]]$SkipFolders, [string]$SkipPattern) {
    $rootFull = (Resolve-Path -LiteralPath $Root).Path.TrimEnd('\')
    $found = New-Object System.Collections.Generic.List[string]
    $pending = New-Object System.Collections.Generic.Stack[string]
    $pending.Push($rootFull)
    while ($pending.Count -gt 0) {
        $folder = $pending.Pop()
        foreach ($entry in Get-ChildItem -LiteralPath $folder -Force) {
            $relative = $entry.FullName.Substring($rootFull.Length + 1).Replace('\', '/')
            if ($entry.PSIsContainer) {
                if ($folder -eq $rootFull -and $SkipFolders -contains $entry.Name) { continue }
                $pending.Push($entry.FullName)
            } elseif ($SkipPattern -eq '' -or -not ($entry.Name -like $SkipPattern)) {
                $found.Add($relative)
            }
        }
    }
    $sorted = $found.ToArray()
    [System.Array]::Sort($sorted, [System.StringComparer]::Ordinal)
    return ,$sorted
}

# Step 2: the hash that identifies the sources a build was made from.
function Get-SourceSha256([string]$Root) {
    $builder = New-Object System.Text.StringBuilder
    foreach ($relative in (Get-RelativeFiles $Root $SkippedFolders $SkippedPattern)) {
        $hash = Get-FileSha256 (Join-Path $Root $relative)
        [void]$builder.Append($relative).Append([char]0).Append($hash).Append("`n")
    }
    return Get-TextSha256 $builder.ToString()
}

# A JSON string literal of a text (the texts here are ASCII paths, versions and hashes).
function ConvertTo-JsonString([string]$Text) {
    return '"' + $Text.Replace('\', '\\').Replace('"', '\"') + '"'
}

# --- 1. Node.js -----------------------------------------------------------------------------------

$node = Get-Command node -ErrorAction SilentlyContinue
$npm = Get-Command npm.cmd -ErrorAction SilentlyContinue
if ($null -eq $node -or $null -eq $npm) {
    Stop-Update 'Node.js (node and npm) is not on PATH. Node.js is needed only to update the browser pages; the normal build does not need it.'
}

# --- 2. The source ----------------------------------------------------------------------------------

if (-not (Test-Path -LiteralPath $Source -PathType Container)) { Stop-Update "The frontend folder $Source does not exist. Pass -Source <folder>." }
$Source = (Resolve-Path -LiteralPath $Source).Path
foreach ($required in @('package.json', 'index.html', 'src')) {
    if (-not (Test-Path -LiteralPath (Join-Path $Source $required))) { Stop-Update "$Source has no $required; it is not the repository's frontend folder." }
}
$sourceSha = Get-SourceSha256 $Source
Write-Step "Source: $Source (source_sha256 $sourceSha)"

if ($Check) {
    if (-not (Test-Path -LiteralPath $VendoredFile)) {
        Write-Step 'web\VENDORED.json does not exist: the browser pages were never built.'
        exit 1
    }
    $vendored = Get-Content -LiteralPath $VendoredFile -Raw | ConvertFrom-Json
    if ($vendored.source_sha256 -eq $sourceSha) {
        Write-Step 'web\dist is current: it was built from these sources.'
        exit 0
    }
    Write-Step "web\dist is out of date: it was built from $($vendored.source_sha256). Run update-web.bat."
    exit 1
}

# --- 3. The copy ------------------------------------------------------------------------------------

if (Test-Path -LiteralPath $WorkDir) { Remove-Item -LiteralPath $WorkDir -Recurse -Force }
New-Item -ItemType Directory -Path $WorkDir -Force | Out-Null
$WorkDir = (Resolve-Path -LiteralPath $WorkDir).Path
$copy = Join-Path $WorkDir 'frontend'
$built = Join-Path $WorkDir 'dist'
Write-Step "Copying the sources to $copy"
& robocopy.exe $Source $copy /E /XD (Join-Path $Source 'node_modules') (Join-Path $Source 'dist') /XF $SkippedPattern /NFL /NDL /NJH /NJS /NP | Out-Null
# robocopy reports success with exit codes below 8.
if ($LASTEXITCODE -ge 8) { Stop-Update "robocopy could not copy the sources (exit code $LASTEXITCODE)." }

if ($UseSourceModules) {
    $modules = Join-Path $Source 'node_modules'
    if (-not (Test-Path -LiteralPath $modules -PathType Container)) { Stop-Update "$Source has no node_modules to copy; run without -UseSourceModules." }
    Write-Step 'Copying node_modules of the source (read only)'
    & robocopy.exe $modules (Join-Path $copy 'node_modules') /E /NFL /NDL /NJH /NJS /NP | Out-Null
    if ($LASTEXITCODE -ge 8) { Stop-Update "robocopy could not copy node_modules (exit code $LASTEXITCODE)." }
} else {
    Write-Step 'npm ci'
    Push-Location $copy
    try {
        & $npm.Source ci --no-audit --no-fund --cache (Join-Path $WorkDir 'npm-cache')
        if ($LASTEXITCODE -ne 0) { Stop-Update "npm ci failed (exit code $LASTEXITCODE). Run again with -UseSourceModules to use the installed packages of the source." }
    } finally {
        Pop-Location
    }
}

# --- 4. The build -----------------------------------------------------------------------------------

Write-Step 'npm run build'
Push-Location $copy
try {
    & $npm.Source run build -- --outDir $built --emptyOutDir
    if ($LASTEXITCODE -ne 0) { Stop-Update "The build failed (exit code $LASTEXITCODE)." }
} finally {
    Pop-Location
}

# --- 5. The output ----------------------------------------------------------------------------------

$index = Join-Path $built 'index.html'
if (-not (Test-Path -LiteralPath $index -PathType Leaf)) { Stop-Update 'The build wrote no index.html.' }
$indexText = Get-Content -LiteralPath $index -Raw
foreach ($match in [regex]::Matches($indexText, '(?:src|href)="/(assets/[^"]+)"')) {
    $asset = $match.Groups[1].Value
    if (-not (Test-Path -LiteralPath (Join-Path $built $asset) -PathType Leaf)) { Stop-Update "index.html references /$asset, which the build did not write." }
}
$licenceIndex = Join-Path $built 'licenses\README.txt'
if (-not (Test-Path -LiteralPath $licenceIndex -PathType Leaf)) { Stop-Update 'The build has no licenses\README.txt.' }
foreach ($match in [regex]::Matches((Get-Content -LiteralPath $licenceIndex -Raw), '(?m)^\s+([A-Za-z0-9._-]+\.txt)\b')) {
    $licence = $match.Groups[1].Value
    if (-not (Test-Path -LiteralPath (Join-Path $built "licenses\$licence") -PathType Leaf)) { Stop-Update "licenses\README.txt names $licence, which the build does not contain." }
}
$files = Get-RelativeFiles $built @() ''
foreach ($relative in $files) {
    if ($relative -ne 'index.html' -and -not ($relative.StartsWith('assets/') -or $relative.StartsWith('licenses/'))) {
        Stop-Update "The build wrote $relative outside assets\ and licenses\; app\build.rs would not know what to do with it."
    }
}

# --- 6. VENDORED.json and web\dist -----------------------------------------------------------------

$viteVersion = (Get-Content -LiteralPath (Join-Path $copy 'node_modules\vite\package.json') -Raw | ConvertFrom-Json).version
$nodeVersion = (& $node.Source --version).Trim()
$npmVersion = (& $npm.Source --version).Trim()
$entries = New-Object System.Collections.Generic.List[string]
foreach ($relative in $files) {
    $path = Join-Path $built $relative
    $entries.Add('    {"path": ' + (ConvertTo-JsonString $relative) + ', "bytes": ' + (Get-Item -LiteralPath $path).Length + ', "sha256": ' + (ConvertTo-JsonString (Get-FileSha256 $path)) + '}')
}
$json = "{`n" +
    '  "source": "instagram-clone/frontend",' + "`n" +
    '  "source_sha256": ' + (ConvertTo-JsonString $sourceSha) + ",`n" +
    '  "built_at": ' + (ConvertTo-JsonString ([DateTime]::UtcNow.ToString('yyyy-MM-ddTHH:mm:ssZ'))) + ",`n" +
    '  "node": ' + (ConvertTo-JsonString $nodeVersion) + ",`n" +
    '  "npm": ' + (ConvertTo-JsonString $npmVersion) + ",`n" +
    '  "vite": ' + (ConvertTo-JsonString $viteVersion) + ",`n" +
    "  `"files`": [`n" + ($entries -join ",`n") + "`n  ]`n}`n"

$before = @{}
if (Test-Path -LiteralPath $DistFolder) {
    foreach ($relative in (Get-RelativeFiles $DistFolder @() '')) { $before[$relative] = Get-FileSha256 (Join-Path $DistFolder $relative) }
}
New-Item -ItemType Directory -Path $WebRoot -Force | Out-Null
$old = $null
if (Test-Path -LiteralPath $DistFolder) {
    $old = Join-Path $WebRoot ('dist.old-' + [DateTime]::UtcNow.ToString('yyyyMMddHHmmss'))
    Rename-Item -LiteralPath $DistFolder -NewName (Split-Path $old -Leaf)
}
Move-Item -LiteralPath $built -Destination $DistFolder
[System.IO.File]::WriteAllText($VendoredFile, $json, (New-Object System.Text.UTF8Encoding($false)))
if ($null -ne $old) { Remove-Item -LiteralPath $old -Recurse -Force }

$changed = 0
foreach ($relative in $files) {
    $hash = Get-FileSha256 (Join-Path $DistFolder $relative)
    if (-not $before.ContainsKey($relative)) { Write-Step "  added    $relative"; $changed++ }
    elseif ($before[$relative] -ne $hash) { Write-Step "  changed  $relative"; $changed++ }
}
foreach ($relative in $before.Keys) {
    if ($files -notcontains $relative) { Write-Step "  removed  $relative"; $changed++ }
}
Write-Step "$changed file(s) changed; web\VENDORED.json written."
if (-not $Keep) { Remove-Item -LiteralPath $WorkDir -Recurse -Force }
Write-Step 'Run build.bat (or cargo build) to embed them.'
exit 0
