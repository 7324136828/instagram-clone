# Builds the portable Debug and Release folders of the application into build\dist.
#
# build.bat runs this script:
#
#   build.bat [--skip-tests] [--profile-only Debug|Release]
#
# What it does, in order:
#
#   1. checks the toolchain (the same checks as setup.bat);
#   2. collects the licence texts of everything that is linked into the exe;
#   3. cargo build --locked for the debug and the release profile;
#   4. cargo test --locked --workspace (unless --skip-tests);
#   5. puts each package together in build\staging-<id>: the exe, README.txt, licenses\;
#   6. validates each package and smoke-tests its exe: "--version" must print the version, and
#      one hidden-window run must render the test export and save a picture of the window;
#   7. replaces the build-owned names in build\dist\Debug and build\dist\Release, and nothing
#      else in those folders; a failure before or during this step leaves build\dist as it was.
#
# Debug is the console build that shows its log; Release is the optimised windowed build.
# Everything the build writes is in build\: cargo's target folder (build\target), the staging
# folder of the build (removed after success, kept after a failure), the pictures of the smoke
# test (build\smoke-test) and the result (build\dist).
#
# The option --package-files <folder> writes only the generated files of a package (README.txt
# of both profiles and licenses\) into <folder> and exits; the tests in app\tests\docs.rs use
# it to check the real output.
#
# The file is plain ASCII on purpose (see setup.ps1).

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

# The toolchain checks and the helpers that start programs.
. (Join-Path $PSScriptRoot 'setup.ps1')

$BuildArguments = @($args)

# The name of the program: the package, the exe and the folder under %APPDATA%.
$AppName = 'instagram-windows-app'
$ExeName = "$AppName.exe"
# The name people see: the window title and the title of README.txt.
$DisplayName = 'Instagram Windows App'
# Where the portable builds are published.
$DistRoot = Join-Path $BuildRoot 'dist'
# The pictures of the last smoke test, one per profile, to look at.
$SmokeRoot = Join-Path $BuildRoot 'smoke-test'
# The profiles in the order they are built, with cargo's folder name and extra arguments.
$Profiles = @(
    @{ Name = 'Debug'; Folder = 'debug'; Arguments = @() },
    @{ Name = 'Release'; Folder = 'release'; Arguments = @('--release') }
)
# The closed list of names the build owns in build\dist\<profile>. Anything else there belongs
# to the user and is never touched.
$Artifacts = @($ExeName, 'README.txt', 'licenses')
# Staging folders of earlier builds are recognised by this name and removed.
$StagePattern = '^staging-[0-9a-f]{32}$'
# The target the licences are collected for.
$WindowsTarget = 'x86_64-pc-windows-msvc'
# No line of a generated text file is longer than this.
$TextColumns = 78
# The export the smoke test opens: six synthetic posts, a carousel and a HEIC picture.
$SmokeFixture = Join-Path $ProjectRoot 'core\tests\fixtures\export-basic'
# The size of the window in the smoke test, in points and (at scale 1) pixels.
$SmokeSize = '1100x720'
# A smoke test that has not finished after this many seconds is stopped and counts as failed.
$SmokeTimeoutSeconds = 180

$Usage = @'
Builds the portable Debug and Release folders of the application into
build\dist.

Usage: build.bat [--skip-tests] [--profile-only Debug|Release]

  --skip-tests             do not run "cargo test --workspace" before
                           publishing
  --profile-only <name>    build and publish only build\dist\Debug or
                           build\dist\Release
  --help                   print this text
'@

# README.txt of a package. {title}, {rule}, {note}, {exe} and {app} are filled in; no other
# brace may be in the text. Every line stays within 78 columns once they are.
$ReadmeTemplate = @'
{title}
{rule}

A viewer for pictures and Instagram exports that looks like Instagram.
This folder runs on 64-bit Windows 10/11 as it is: there is nothing to
install, and {exe} is the whole program. The
licenses folder holds the licence texts of the software inside it; keep
it beside the exe when you pass the folder on.

{note}

Start
-----
  {exe}                  an empty window
  {exe} <picture>        the picture and its folder
  {exe} <folder>         the pictures in that folder
  {exe} <archive>        an export (.zip, .tar, .tar.gz)
  {exe} --help           options and keyboard keys

Double-click the exe, drag pictures or a folder onto it, or drop them on
its window. In the window: Open pictures (Ctrl+O) and Open folder
(Ctrl+Shift+O).

Opening pictures
----------------
One picture opens together with the other pictures of its folder, and the
viewer starts on it. Several pictures open on their own. A folder opens
the pictures directly in it, not those in its subfolders. A folder that
holds a post.json, or that file itself, opens as an Instagram export with
its authors, descriptions, counts and carousels; so does a folder without
pictures of its own whose subfolder is an export (an extracted download).

An export archive (.zip, .tar, .tar.gz or .tgz) opens like an export
folder; its post.json may be at the top or inside one folder. A folder
that holds only archives opens those archives. Several exports and
archives opened together become one feed, newest first; a post that is
in two of them is shown once, from the one opened last. Opening more
exports or archives while exports are shown adds them. The pictures of
an archive are unpacked into the archive cache (see below), never next
to the archive.

JPEG, PNG, GIF, WebP and BMP are decoded by the program itself. HEIC,
HEIF and AVIF are decoded by Windows and need the "HEIF Image Extensions"
from the Microsoft Store, together with the "HEVC Video Extensions"
(HEIC) or the "AV1 Video Extension" (AVIF).

Videos (MP4, MOV, WebM) play in the viewer, and in the feed with a
graphics card (without one the feed shows the first frame with "Play
video"). They open like pictures, alone or with the pictures of their
folder, and the videos of exports and archives play too. Windows plays
them; HEVC and VP9 videos need the "HEVC Video Extensions" or the "VP9
Video Extensions" from the Microsoft Store. The posters of loose videos
are kept in the poster cache (see below), never next to the video.

Browser pages
-------------
What is open can also be seen in a web browser, with the pages of the
Instagram clone: More > Open in browser opens http://127.0.0.1:3127/,
and More > About and More > Browser view show the addresses. Browser
view also sets the port of the page address (3127) and the port of the
API address (8317) and saves them in settings.json. The pages are
served to this computer only (127.0.0.1) unless you turn on "Allow
other devices on this network" in Browser view; Windows Defender
Firewall then usually asks whether to let the program through (allow it
for private networks only). That switch is saved: every later start
listens on the network too and says so. An edit made in the browser is
saved in descriptions.json, like one made here; an import from the
browser goes to the browser imports folder (see below). The browser
never changes or deletes your pictures, folders or archives. Where the
pages say where imports are kept or saved ("saved in ...", also in
their banner) and in their "Clear library" question, they name the
browser imports folder, never your own exports; with loose pictures
open they name the pictures' folder, and imports and Clear library are
refused. Reload the page to see what changed here since it was loaded.

Open with
---------
To open pictures from Explorer, right-click a picture, choose "Open with"
and "Choose another app", then browse to {exe}
in this folder ("Choose an app on your PC" on Windows 11, "Look for
another app on this PC" on Windows 10). Choose "Always" to make it the
viewer for that file type. Windows remembers the path of the exe, so
choose it again after you move this folder.

Settings and descriptions
-------------------------
  %APPDATA%\{app}\settings.json    your settings
  %APPDATA%\{app}\data\            the data folder
  %LOCALAPPDATA%\{app}\archives\   the archive cache
  %LOCALAPPDATA%\{app}\library\    the browser imports
  %LOCALAPPDATA%\{app}\posters\    the posters of loose videos
  %LOCALAPPDATA%\{app}\web-cache\  JPEGs of HEIC pictures

The archive cache holds what was unpacked of the archives you opened. It
cleans itself up: at most 8 GB, nothing unused for 30 days, and 1 GB of
the drive stays free.
The web cache holds JPEG copies of HEIC pictures for the browser pages,
which show no HEIC: at most 1 GB, the oldest go first, and it can be
deleted while the program is closed.

settings.json is created on the first start. The program writes to it
only when you choose Save in Browser view, and then changes only the
four keys of the browser pages. Its keys:

  "data_directory"      another folder for the data; %VARIABLES% and a
                        leading ~ are expanded, a relative path counts
                        from the folder of settings.json
  "default_username"    the name shown as the author of your pictures
                        (default: your Windows account name)
  "web_server"          false: no browser pages (default: true)
  "frontend_port"       the port of the page address (default: 3127)
  "backend_port"        the port of the API address (default: 8317)
  "web_host"            "0.0.0.0" for other devices on the network
                        (default: "127.0.0.1", this computer only)
  "allowed_hosts"       further host names the pages answer to
  "library_directory"   another folder for the browser imports

The data folder holds:

  descriptions.json     the descriptions you write with Edit description
  state.json            the appearance, the auto view settings, the folder
                        opened last and the folder of the last download
  window.ron            the size and position of the window
  app.log               the log

The program never writes into the folders of your pictures and never
changes an opened export or archive. The descriptions you write exist only in
descriptions.json, so include that file in your backups. Download post
and Download all write an Instagram export (.zip) that holds the pictures
with their descriptions.

Command line
------------
  --autoview[=SECONDS]  start auto view at once, with 2 to 3600 seconds
                        per post (without a number: the time used
                        last)
  --loop=0|1            with --autoview: start again after the last
                        post (1) or stop there (0)
  --shuffle=0|1         with --autoview: random order (1) or not (0)
  --wait=0|1            with --autoview: let a long video play to its
                        end (1) or not (0)
  --help                all options and the keyboard shortcuts
  --version             the version of this build
  --no-web              no browser pages in this run
  --host ADDRESS        listen on this IPv4 address in this run
  --frontend-port PORT  the first port tried for the page address
  --backend-port PORT   the first port tried for the API address

Licences
--------
The program is licensed under the Apache License 2.0. It contains
open-source libraries and fonts under their own licences:
licenses\README.txt lists them, and their texts are beside it. The
browser pages contain React, fflate and heic-to (LGPL-3.0); their list is
licenses\web\README.txt.

Files you add to this folder are kept when the build is updated.
'@

# What tells the two profiles apart, as the paragraph under the introduction of README.txt.
$ProfileNotes = @{
    'Debug'   = "Debug profile: a console window opens with the program and shows its log.`nThe program runs with extra checks and is slower. Use it to find out why`nsomething fails."
    'Release' = "Release profile: optimised, without a console window. Warnings and errors`nare written to app.log in the data folder (see below)."
}

# The fonts that the egui library embeds, with the licence files of its font crate. A font is
# only inside the exe when the feature of the crate named here is switched on (an empty
# feature: always, when the font crate is part of the build).
$FontCrate = 'epaint_default_fonts'
$BundledFonts = @(
    @{ Name = 'Ubuntu Light'; Licence = 'Ubuntu-font-1.0'; Url = 'https://design.ubuntu.com/font'; Files = @('UFL.txt'); Crate = 'epaint'; Feature = 'default_fonts' },
    @{ Name = 'Hack'; Licence = 'MIT AND Bitstream-Vera'; Url = 'https://github.com/source-foundry/Hack'; Files = @('Hack-Regular.txt'); Crate = 'epaint'; Feature = 'default_fonts' },
    @{ Name = 'egui-icons (five glyphs of emoji-icon-font)'; Licence = 'MIT'; Url = 'https://github.com/jslegers/emoji-icon-font'; Files = @('egui-icons.txt', 'emoji-icon-font-mit-license.txt'); Crate = $FontCrate; Feature = '' },
    @{ Name = 'Noto Emoji'; Licence = 'OFL-1.1'; Url = 'https://fonts.google.com/noto/specimen/Noto+Emoji'; Files = @('OFL.txt'); Crate = $FontCrate; Feature = 'monochrome_emoji_fonts' },
    @{ Name = 'emoji-icon-font'; Licence = 'MIT'; Url = 'https://github.com/jslegers/emoji-icon-font'; Files = @('emoji-icon-font-mit-license.txt'); Crate = $FontCrate; Feature = 'monochrome_emoji_fonts' }
)
# Licences in the expression of the font crate that belong to its fonts, not to its code; their
# texts are the font files above.
$FontLicences = @('OFL-1.1', 'Ubuntu-font-1.0')
# The files of a crate that are licence texts or notices, by the start of their name.
$LicenceFilePattern = '^(LICEN[CS]E|COPYING|COPYRIGHT|NOTICE|UNLICEN[CS]E|AUTHORS|PATENTS)'
# A folder of a crate that holds licence texts.
$LicenceFolderPattern = '^LICEN[CS]ES?$'

# Stops the build with a one-line message; the entry point prints it as "[build] ERROR: ...".
function Stop-Build {
    param([string]$Message)
    throw $Message
}

# Why a .NET call failed: PowerShell wraps the real exception in one of its own.
function Get-ErrorReason {
    param($ErrorRecord)
    $exception = $ErrorRecord.Exception
    if ($exception.InnerException) { return $exception.InnerException.Message }
    return $exception.Message
}

# The version of the workspace, from [workspace.package] of Cargo.toml.
function Get-ProjectVersion {
    $manifest = [System.IO.File]::ReadAllText((Join-Path $ProjectRoot 'Cargo.toml'))
    $match = [regex]::Match($manifest, '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"')
    if (-not $match.Success) {
        Stop-Build 'Cargo.toml has no version under [workspace.package]'
    }
    return $match.Groups[1].Value
}

# Writes a generated text file: plain ASCII with CRLF line ends, as Notepad of any Windows
# version shows it. A character outside ASCII or a line over 78 columns is a mistake in the
# text and stops the build; single words that cannot be broken (a web address, a path) may be
# longer.
function Write-TextFile {
    param([string]$Path, [string]$Text)
    $lines = ($Text -replace "`r`n", "`n" -replace "`r", "`n").TrimEnd("`n") -split "`n"
    $number = 0
    foreach ($line in $lines) {
        $number++
        if ($line -cmatch '[^\x20-\x7E]') {
            Stop-Build "Line $number of $Path has a character outside ASCII: $line"
        }
        if ($line.Length -gt $TextColumns -and $line.Trim() -match '\s') {
            Stop-Build "Line $number of $Path is longer than $TextColumns columns: $line"
        }
    }
    $folder = Split-Path -Parent $Path
    if (-not (Test-Path -LiteralPath $folder)) { [void](New-Item -ItemType Directory -Path $folder -Force) }
    [System.IO.File]::WriteAllText($Path, (($lines -join "`r`n") + "`r`n"), (New-Object System.Text.ASCIIEncoding))
}

# The text of README.txt for one profile.
function Get-PackageReadme {
    param([string]$Profile, [string]$Version)
    $title = "$DisplayName $Version - portable $Profile build"
    $text = $ReadmeTemplate.Replace('{title}', $title).Replace('{rule}', ('=' * $title.Length))
    $text = $text.Replace('{note}', $ProfileNotes[$Profile]).Replace('{exe}', $ExeName).Replace('{app}', $AppName)
    if ($text -match '[{}]') {
        Stop-Build 'The README.txt template has a placeholder that was not filled in'
    }
    return $text
}

# Text reduced to ASCII for the licence index: accents are dropped ("Roessler" stays readable
# as "Rossler"), anything else outside ASCII becomes a question mark.
function ConvertTo-Ascii {
    param([string]$Text)
    $decomposed = $Text.Normalize([System.Text.NormalizationForm]::FormD)
    $builder = New-Object System.Text.StringBuilder
    foreach ($character in $decomposed.ToCharArray()) {
        $category = [System.Globalization.CharUnicodeInfo]::GetUnicodeCategory($character)
        if ($category -eq [System.Globalization.UnicodeCategory]::NonSpacingMark) { continue }
        if ([int]$character -ge 0x20 -and [int]$character -le 0x7E) { [void]$builder.Append($character) } else { [void]$builder.Append('?') }
    }
    return $builder.ToString()
}

# Words wrapped to the text width, every line indented by $Indent spaces.
function Format-Paragraph {
    param([string]$Text, [int]$Indent)
    $lines = @()
    $line = ''
    foreach ($word in ($Text -split '\s+' | Where-Object { $_ -ne '' })) {
        if ($line -ne '' -and ($Indent + $line.Length + 1 + $word.Length) -gt $TextColumns) {
            $lines += (' ' * $Indent) + $line
            $line = $word
        } elseif ($line -eq '') {
            $line = $word
        } else {
            $line = "$line $word"
        }
    }
    if ($line -ne '') { $lines += (' ' * $Indent) + $line }
    return $lines
}

# The first line of a block of the licence index: the name at the left edge and the licence at
# the right edge; a licence too long for that goes on a line of its own.
function Format-IndexHeading {
    param([string]$Name, [string]$Licence)
    if ($Licence -eq '') { return @($Name) }
    $gap = $TextColumns - $Name.Length - $Licence.Length
    if ($gap -ge 2) { return @($Name + (' ' * $gap) + $Licence) }
    return @($Name, ((' ' * [Math]::Max(0, $TextColumns - $Licence.Length)) + $Licence))
}

# The identifiers of an SPDX licence expression: "(MIT OR Apache-2.0) AND Unicode-3.0" names
# MIT, Apache-2.0 and Unicode-3.0. The old form "MIT/Apache-2.0" is read as well.
function Get-LicenceIdentifiers {
    param([string]$Expression)
    $words = ($Expression -replace '[()/]', ' ') -split '\s+' | Where-Object { $_ -ne '' -and $_ -cnotmatch '^(OR|AND|WITH)$' }
    return @($words | Select-Object -Unique)
}

# The names and versions of the crates in the Windows build of the exe, from "cargo tree".
# Crates that only run while the program is compiled (procedural macros and what they need) are
# part of the list; build scripts and test tools are not.
function Get-LinkedCrates {
    param([string]$Cargo)
    $arguments = @('tree', '--locked', '-p', $AppName, '--target', $WindowsTarget, '-e', 'normal', '--prefix', 'none', '--format', '{p}')
    $result = Start-Tool -FilePath $Cargo -Arguments $arguments -Capture
    if ($result.ExitCode -ne 0) { Stop-Build "cargo tree failed with exit code $($result.ExitCode)" }
    $crates = @{}
    foreach ($line in ($result.Output -split "`r?`n")) {
        if ($line -match '^(\S+) v(\S+)') { $crates["$($Matches[1]) $($Matches[2])"] = $true }
    }
    if ($crates.Count -eq 0) { Stop-Build 'cargo tree listed no crates' }
    return @($crates.Keys | Sort-Object)
}

# "cargo metadata" for the Windows build, parsed. Windows PowerShell's ConvertFrom-Json gives
# up on a document of this size, the .NET serialiser behind it does not.
function Get-CargoMetadata {
    param([string]$Cargo)
    $arguments = @('metadata', '--locked', '--format-version', '1', '--filter-platform', $WindowsTarget)
    $result = Start-Tool -FilePath $Cargo -Arguments $arguments -Capture
    if ($result.ExitCode -ne 0) { Stop-Build "cargo metadata failed with exit code $($result.ExitCode)" }
    Add-Type -AssemblyName System.Web.Extensions
    $serializer = New-Object System.Web.Script.Serialization.JavaScriptSerializer
    $serializer.MaxJsonLength = [int]::MaxValue
    return $serializer.DeserializeObject($result.Output)
}

# Collects the licence texts of everything inside the exe into $Destination and writes the
# index README.txt there:
#
#   README.txt                     which text belongs to which package
#   LICENSE.txt                    the licence of the program itself
#   crates\<name>-<version>\       the licence files each crate is published with, verbatim
#   standard\<SPDX id>.txt         the standard text of a licence, for crates that ship none
#   fonts\                         the licence files of the fonts embedded by egui
#   web\                           the licence folder of the browser pages (web\dist\licenses)
#
# A crate without a licence file whose licence has no text in licenses\standard stops the
# build: add that text first.
function Write-LicenceFolder {
    param([string]$Cargo, [string]$Destination, [string]$Version)
    $standardSource = Join-Path $ProjectRoot 'licenses\standard'
    $crates = Get-LinkedCrates $Cargo
    $metadata = Get-CargoMetadata $Cargo
    $packages = @{}
    foreach ($package in $metadata['packages']) { $packages["$($package['name']) $($package['version'])"] = $package }
    # The features each crate is compiled with, by crate name.
    $namesById = @{}
    foreach ($package in $metadata['packages']) { $namesById[$package['id']] = $package['name'] }
    $features = @{}
    foreach ($node in $metadata['resolve']['nodes']) { $features[$namesById[$node['id']]] = @($node['features']) }

    if (Test-Path -LiteralPath $Destination) { Remove-Item -LiteralPath $Destination -Recurse -Force }
    [void](New-Item -ItemType Directory -Path $Destination -Force)
    $usedStandard = @{}
    $useStandard = {
        param([string]$Identifier)
        $source = Join-Path $standardSource "$Identifier.txt"
        if (-not (Test-Path -LiteralPath $source)) { return $false }
        if (-not $usedStandard.ContainsKey($Identifier)) {
            $target = Join-Path $Destination "standard\$Identifier.txt"
            [void](New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force)
            Copy-Item -LiteralPath $source -Destination $target
            $usedStandard[$Identifier] = $true
        }
        return $true
    }

    $index = @()
    $index += 'Third-party software in this build'
    $index += '=================================='
    $index += ''
    $index += Format-Paragraph "$ExeName is built from the Rust crates listed below and contains a few fonts. The compiled program no longer shows their licence notices, so the licence texts travel in this folder. Each block names a package and its version, its licence as an SPDX expression at the right edge, where the package comes from, and the files in this folder that belong to it." 0
    $index += ''
    $index += Format-Paragraph 'A crate that is offered under several licences ("OR") comes with all of its texts. A few crates only run while the program is compiled, where they generate code; they are listed all the same.' 0
    $index += ''
    $index += 'This program'
    $index += '------------'
    $index += ''
    $index += Format-IndexHeading "$AppName $Version" 'Apache-2.0'
    $index += Format-Paragraph "The viewer itself: the packages $AppName and instagram-core." 4
    $index += '      LICENSE.txt'
    Copy-Item -LiteralPath (Join-Path $ProjectRoot 'LICENSE') -Destination (Join-Path $Destination 'LICENSE.txt')
    $index += ''
    $index += 'Runtime libraries'
    $index += '-----------------'
    $index += ''
    $index += Format-IndexHeading 'Rust standard library' 'MIT OR Apache-2.0'
    $index += '    https://github.com/rust-lang/rust'
    $index += Format-Paragraph 'Linked into every Rust program.' 4
    foreach ($identifier in @('MIT', 'Apache-2.0')) {
        if (-not (& $useStandard $identifier)) { Stop-Build "licenses\standard\$identifier.txt is missing" }
        $index += "      standard/$identifier.txt"
    }
    $index += ''
    $index += Format-IndexHeading 'Microsoft C runtime' ''
    $index += Format-Paragraph 'The static C runtime of the Visual Studio C++ build tools is linked into the exe, so that no Visual C++ Redistributable has to be installed. It is redistributed under the licence terms of Visual Studio.' 4

    # The browser pages: their own licence folder, copied as it is.
    $webLicences = Join-Path $ProjectRoot 'web\dist\licenses'
    if (-not (Test-Path -LiteralPath (Join-Path $webLicences 'README.txt'))) { Stop-Build "web\dist\licenses\README.txt is missing; run update-web.bat or restore web\dist" }
    $index += ''
    $index += 'Browser pages'
    $index += '-------------'
    $index += ''
    $index += Format-Paragraph 'The browser pages (web\dist of the project, built from the frontend of the Instagram clone) are embedded in the exe and served to the browser. They contain heic-to (LGPL-3.0, with libheif and libde265 compiled to JavaScript; the separate file assets/heic-to-<hash>.js), fflate (MIT) and React, react-dom and scheduler (MIT). web/README.txt is their own list.' 0
    $index += ''
    $index += Format-Paragraph 'web/README.txt is the list of the Instagram clone, whose own build keeps the pages in a folder of their own; in this program they are inside the exe. To use another or a modified version of heic-to here, change the heic-to dependency of the clone''s frontend, run update-web.bat and then build.bat in the source code of this program (see "Third-party software" in its README.md). The texts of the LGPL and of the GPL it incorporates are web/heic-to.LICENSE.txt and web/GPL-3.0.txt.' 0
    $index += ''
    $index += Format-IndexHeading 'Browser pages' 'LGPL-3.0 AND MIT'
    $index += '    https://github.com/hoppergee/heic-to'
    $index += '    https://github.com/101arrowz/fflate'
    $index += '    https://github.com/facebook/react'
    foreach ($file in (Get-ChildItem -LiteralPath $webLicences -File | Sort-Object Name)) {
        $target = Join-Path $Destination "web\$($file.Name)"
        [void](New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force)
        Copy-Item -LiteralPath $file.FullName -Destination $target -Force
        $index += "      web/$($file.Name)"
    }

    # Fonts.
    $fontLines = @()
    $fontPackage = $null
    foreach ($key in $crates) {
        if ($packages.ContainsKey($key) -and $packages[$key]['name'] -eq $FontCrate) { $fontPackage = $packages[$key] }
    }
    if ($fontPackage) {
        $fontSource = Join-Path (Split-Path -Parent $fontPackage['manifest_path']) 'fonts'
        foreach ($font in $BundledFonts) {
            $enabled = $features.ContainsKey($font.Crate) -and ($font.Feature -eq '' -or $features[$font.Crate] -contains $font.Feature)
            if (-not $enabled) { continue }
            $fontLines += ''
            $fontLines += Format-IndexHeading $font.Name $font.Licence
            $fontLines += "    $($font.Url)"
            foreach ($file in $font.Files) {
                $source = Join-Path $fontSource $file
                if (-not (Test-Path -LiteralPath $source)) { Stop-Build "The licence file $file of the font $($font.Name) is missing in $fontSource" }
                $target = Join-Path $Destination "fonts\$file"
                [void](New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force)
                Copy-Item -LiteralPath $source -Destination $target -Force
                $fontLines += "      fonts/$file"
            }
        }
    }
    $index += ''
    $index += 'Fonts'
    $index += '-----'
    $index += ''
    if ($fontLines.Count -gt 0) {
        $index += Format-Paragraph 'Segoe UI and the other fonts of Windows are read from the Windows fonts folder when the program starts; they are not part of this build. These fonts are inside the exe, they come with the egui library:' 0
        $index += $fontLines
    } else {
        $index += Format-Paragraph 'No font is inside the exe: Segoe UI and the other fonts of Windows are read from the Windows fonts folder when the program starts.' 0
    }

    # Crates.
    $index += ''
    $index += 'Rust crates'
    $index += '-----------'
    $count = 0
    $workspaceMembers = @($metadata['workspace_members'])
    foreach ($key in $crates) {
        if (-not $packages.ContainsKey($key)) { Stop-Build "cargo metadata does not know the crate $key that cargo tree lists" }
        $package = $packages[$key]
        # The packages of this workspace are the program itself. A crate in vendor\ has no
        # registry source either, but it is third-party software like the others.
        if ($workspaceMembers -contains $package['id']) { continue }
        $vendored = -not $package['source']
        $count++
        $name = $package['name']
        $folderName = "$name-$($package['version'])"
        $licence = [string]$package['license']
        if ($licence -eq '' -and -not $package['license_file']) { Stop-Build "The crate $key declares no licence" }
        $source = Split-Path -Parent $package['manifest_path']
        if (-not (Test-Path -LiteralPath $source)) { Stop-Build "The source of the crate $key is not in the cargo registry ($source); run setup.bat" }
        $files = @()
        foreach ($file in (Get-ChildItem -LiteralPath $source -File | Where-Object { $_.Name -match $LicenceFilePattern } | Sort-Object Name)) {
            $files += @{ Source = $file.FullName; Relative = $file.Name }
        }
        foreach ($folder in (Get-ChildItem -LiteralPath $source -Directory | Where-Object { $_.Name -match $LicenceFolderPattern } | Sort-Object Name)) {
            foreach ($file in (Get-ChildItem -LiteralPath $folder.FullName -File | Sort-Object Name)) {
                $files += @{ Source = $file.FullName; Relative = "$($folder.Name)\$($file.Name)" }
            }
        }
        if ($package['license_file']) {
            $declared = Join-Path $source $package['license_file']
            $known = $files | Where-Object { $_.Source -eq $declared }
            if ((Test-Path -LiteralPath $declared) -and -not $known) { $files += @{ Source = $declared; Relative = (Split-Path -Leaf $declared) } }
        }

        $index += ''
        $index += Format-IndexHeading (ConvertTo-Ascii "$name $($package['version'])") (ConvertTo-Ascii $licence)
        $address = $package['repository']
        if (-not $address) { $address = $package['homepage'] }
        if (-not $address) { $address = "https://crates.io/crates/$name" }
        $index += "    $(ConvertTo-Ascii $address)"
        if ($vendored) {
            $index += Format-Paragraph 'Built from the copy in the folder vendor of the source code, which changes its shader for computers without a graphics card (vendor/README.md there).' 4
        }
        if ($files.Count -gt 0) {
            foreach ($file in $files) {
                $target = Join-Path $Destination "crates\$folderName\$($file.Relative)"
                [void](New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force)
                Copy-Item -LiteralPath $file.Source -Destination $target -Force
                $index += "      crates/$folderName/$($file.Relative -replace '\\', '/')"
            }
        } else {
            # The crate is published without a licence file: the standard texts of its licence
            # stand in, and the index names its authors, as the licences ask for.
            $authors = @($package['authors'] | ForEach-Object { ($_ -replace '\s*<[^>]*>', '').Trim() } | Where-Object { $_ -ne '' })
            $note = 'The crate is published without a licence file; the standard text applies.'
            if ($authors.Count -gt 0) { $note = "$note Authors: $((ConvertTo-Ascii ($authors -join ', ')))." }
            $index += Format-Paragraph $note 4
            $covered = 0
            foreach ($identifier in (Get-LicenceIdentifiers $licence)) {
                if (& $useStandard $identifier) {
                    $index += "      standard/$identifier.txt"
                    $covered++
                } elseif ($name -eq $FontCrate -and $FontLicences -contains $identifier) {
                    $covered++
                } else {
                    Stop-Build "No licence text for the crate $key ($licence): it ships no licence file and licenses\standard\$identifier.txt does not exist. Add that file."
                }
            }
            if ($covered -eq 0) { Stop-Build "No licence text for the crate $key ($licence)" }
            if ($name -eq $FontCrate) { $index += Format-Paragraph 'The licences of its fonts are listed under Fonts.' 4 }
        }
    }
    Write-TextFile (Join-Path $Destination 'README.txt') ($index -join "`n")
    return $count
}

# Removes the staging folders that earlier builds left behind (a failed build keeps its folder
# for troubleshooting until the next build).
function Remove-StaleStages {
    if (-not (Test-Path -LiteralPath $BuildRoot)) { return }
    foreach ($entry in (Get-ChildItem -LiteralPath $BuildRoot -Directory | Where-Object { $_.Name -match $StagePattern })) {
        try {
            Remove-Item -LiteralPath $entry.FullName -Recurse -Force
        } catch {
            Write-Step 'build' "Warning: could not remove old build files in $($entry.FullName)"
        }
    }
}

# Checks one staged package and smoke-tests its exe.
function Test-Package {
    param([string]$Package, [string]$Profile, [string]$Version, [string]$Work)
    $exe = Join-Path $Package $ExeName
    if (-not (Test-Path -LiteralPath $exe)) { Stop-Build "cargo did not produce $ExeName for the $Profile profile" }
    foreach ($text in @((Join-Path $Package 'README.txt'), (Join-Path $Package 'licenses\README.txt'))) {
        if (-not (Test-Path -LiteralPath $text) -or (Get-Item -LiteralPath $text).Length -eq 0) { Stop-Build "Missing package file $text" }
    }
    $readme = [System.IO.File]::ReadAllText((Join-Path $Package 'README.txt'))
    if (-not $readme.Contains("portable $Profile build") -or -not $readme.Contains("%APPDATA%\$AppName\settings.json")) {
        Stop-Build "Unexpected README.txt in $Package"
    }

    # The exe answers --version by itself, without a window and without touching the settings.
    Write-Step 'build' ('$ ' + (Format-CommandLine @("$Profile\$ExeName", '--version')))
    $answer = Start-Tool -FilePath $exe -Arguments @('--version') -Quiet -TimeoutSeconds 60
    $printed = $answer.Output.Trim()
    if ($answer.TimedOut -or $answer.ExitCode -ne 0 -or $printed -ne "$AppName $Version") {
        Stop-Build "The $Profile exe did not answer --version as expected (exit code $($answer.ExitCode), output `"$printed`")"
    }
    Write-Step 'build' $printed

    # The exe renders: a hidden window on a profile of its own opens the test export and saves
    # a picture of itself. The user's settings and data are not touched.
    $profileFolder = Join-Path $Work 'profile'
    $picture = Join-Path $Work 'window.png'
    $arguments = @('--profile-dir', $profileFolder, '--screenshot', $picture, '--size', $SmokeSize, '--scale', '1.0', '--watchdog', '120', $SmokeFixture)
    Write-Step 'build' ('$ ' + (Format-CommandLine (@("$Profile\$ExeName") + $arguments)))
    $run = Start-Tool -FilePath $exe -Arguments $arguments -Quiet -TimeoutSeconds $SmokeTimeoutSeconds
    $rendered = (Test-Path -LiteralPath $picture) -and ((Get-Item -LiteralPath $picture).Length -gt 0)
    if ($run.TimedOut -or $run.ExitCode -ne 0 -or -not $rendered) {
        $tail = (($run.Errors + $run.Output) -split "`r?`n" | Where-Object { $_.Trim() -ne '' } | Select-Object -Last 15) -join "`n"
        Stop-Build "The $Profile exe did not render in the smoke test (exit code $($run.ExitCode), timed out: $($run.TimedOut)). Its last output:`n$tail"
    }
    $summary = ($run.Errors -split "`r?`n" | Where-Object { $_ -match '^SCREENSHOT OK' } | Select-Object -Last 1)
    if (-not $summary) { $summary = 'SCREENSHOT OK' }
    [void](New-Item -ItemType Directory -Path $SmokeRoot -Force)
    $kept = Join-Path $SmokeRoot "$Profile.png"
    Copy-Item -LiteralPath $picture -Destination $kept -Force
    $details = $summary.Replace('SCREENSHOT OK', '').Replace($picture, '').Trim()
    Write-Step 'build' "Smoke test passed: the $Profile exe rendered build\smoke-test\$Profile.png ($details)"
}

# Fails when a file that is about to be replaced is open in another program. Windows lets a
# running exe be renamed but not opened for writing, so this finds a running application
# before anything is moved.
function Assert-Replaceable {
    param([string]$Path)
    $files = @()
    if (Test-Path -LiteralPath $Path -PathType Container) {
        $files = @(Get-ChildItem -LiteralPath $Path -Recurse -File | ForEach-Object { $_.FullName })
    } elseif (Test-Path -LiteralPath $Path) {
        $files = @($Path)
    }
    foreach ($file in $files) {
        try {
            $stream = [System.IO.File]::Open($file, [System.IO.FileMode]::Open, [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::None)
            $stream.Dispose()
        } catch {
            Stop-Build "$file is in use, probably by the running application. Close it and retry. ($(Get-ErrorReason $_))"
        }
    }
}

# Moves a file or a folder; both stay on the drive of build\, so this is a rename.
function Move-Entry {
    param([string]$From, [string]$To)
    if (Test-Path -LiteralPath $From -PathType Container) {
        [System.IO.Directory]::Move($From, $To)
    } else {
        [System.IO.File]::Move($From, $To)
    }
}

# Publishes the staged packages: in build\dist\<profile> only the names in $Artifacts are
# replaced; anything else there, such as notes or pictures the user put beside the exe, is
# never touched. Everything is checked before the first move, and a failure while moving puts
# every profile back as it was.
function Publish-Packages {
    param([string]$Stage, [string[]]$Names)
    foreach ($name in $Names) {
        foreach ($artifact in $Artifacts) {
            $source = Join-Path $Stage "packages\$name\$artifact"
            if (-not (Test-Path -LiteralPath $source)) { Stop-Build "The staged package is incomplete: $source is missing" }
            Assert-Replaceable (Join-Path $DistRoot "$name\$artifact")
        }
    }
    $moves = New-Object System.Collections.ArrayList
    try {
        foreach ($name in $Names) {
            $target = Join-Path $DistRoot $name
            $previous = Join-Path $Stage "previous\$name"
            [void](New-Item -ItemType Directory -Path $target -Force)
            [void](New-Item -ItemType Directory -Path $previous -Force)
            foreach ($artifact in $Artifacts) {
                $source = Join-Path $Stage "packages\$name\$artifact"
                $published = Join-Path $target $artifact
                $backup = Join-Path $previous $artifact
                if (Test-Path -LiteralPath $published) {
                    Move-Entry $published $backup
                    [void]$moves.Add(@{ From = $published; To = $backup })
                }
                Move-Entry $source $published
                [void]$moves.Add(@{ From = $source; To = $published })
            }
        }
    } catch {
        $reason = Get-ErrorReason $_
        for ($index = $moves.Count - 1; $index -ge 0; $index--) {
            try { Move-Entry $moves[$index].To $moves[$index].From } catch { }
        }
        Stop-Build "Could not replace the $($Names -join '/') artifacts; close the running application and retry. $reason"
    }
}

# "21.4 MB": the size of a file for the last lines of the build.
function Format-Size {
    param([string]$Path)
    return ('{0:N1} MB' -f ((Get-Item -LiteralPath $Path).Length / 1MB))
}

# "12 min 05 s": how long the build took.
function Format-Duration {
    param([TimeSpan]$Duration)
    return ('{0} min {1:00} s' -f [int][Math]::Floor($Duration.TotalMinutes), $Duration.Seconds)
}

Invoke-Main 'build' {
    $skipTests = $false
    $only = $null
    $packageFiles = $null
    for ($position = 0; $position -lt $BuildArguments.Count; $position++) {
        $argument = [string]$BuildArguments[$position]
        if ($argument -ceq '--skip-tests') {
            $skipTests = $true
        } elseif ($argument -ceq '--profile-only') {
            $position++
            $value = ''
            if ($position -lt $BuildArguments.Count) { $value = [string]$BuildArguments[$position] }
            $only = $Profiles | Where-Object { $_.Name -eq $value } | Select-Object -First 1
            if (-not $only) { Stop-Build "--profile-only expects Debug or Release, not `"$value`"" }
        } elseif ($argument -ceq '--package-files') {
            $position++
            if ($position -ge $BuildArguments.Count) { Stop-Build '--package-files needs a folder' }
            $packageFiles = [string]$BuildArguments[$position]
        } elseif ($argument -ceq '--help') {
            [Console]::Out.WriteLine($Usage)
            return
        } else {
            Stop-Build "Unknown option `"$argument`". Usage: build.bat [--skip-tests] [--profile-only Debug|Release]"
        }
    }
    $version = Get-ProjectVersion

    if ($packageFiles) {
        # Only the generated files, for the tests: no compiler is needed for them.
        $cargo = Find-Cargo 'build'
        $folder = [System.IO.Path]::GetFullPath($packageFiles)
        foreach ($entry in $Profiles) {
            Write-TextFile (Join-Path $folder "$($entry.Name)\README.txt") (Get-PackageReadme $entry.Name $version)
        }
        $count = Write-LicenceFolder $cargo (Join-Path $folder 'licenses') $version
        Write-Step 'build' "Wrote README.txt of both profiles and the licences of $count crates to $folder"
        return
    }

    $started = Get-Date
    $toolchain = Test-Toolchain 'build'
    $cargo = $toolchain.Cargo
    $selected = $Profiles
    if ($only) { $selected = @($only) }
    $names = @($selected | ForEach-Object { $_.Name })
    if (-not (Test-Path -LiteralPath (Join-Path $SmokeFixture 'post.json'))) {
        Stop-Build "The export of the smoke test is missing: $SmokeFixture"
    }

    [void](New-Item -ItemType Directory -Path $BuildRoot -Force)
    Remove-StaleStages
    $stage = Join-Path $BuildRoot ('staging-' + [guid]::NewGuid().ToString('N'))
    [void](New-Item -ItemType Directory -Path $stage)
    try {
        Write-Step 'build' 'Collecting the licence texts of the crates and fonts in the exe'
        $licences = Join-Path $stage 'licenses'
        $count = Write-LicenceFolder $cargo $licences $version
        Write-Step 'build' "Licences: $count crates, listed in licenses\README.txt"

        # cargo says where its target folder is (.cargo\config.toml: build\target).
        $located = Start-Tool -FilePath $cargo -Arguments @('metadata', '--locked', '--no-deps', '--format-version', '1') -Capture
        $targetMatch = [regex]::Match($located.Output, '"target_directory"\s*:\s*"((?:[^"\\]|\\.)*)"')
        if ($located.ExitCode -ne 0 -or -not $targetMatch.Success) { Stop-Build 'cargo metadata did not name the target folder' }
        $targetRoot = [System.IO.Path]::GetFullPath(($targetMatch.Groups[1].Value -replace '\\\\', '\'))

        foreach ($entry in $selected) {
            Invoke-Step 'build' $cargo (@('build', '--locked', '-p', $AppName) + $entry.Arguments)
            # Taken right away: the tests link the debug exe again with the features of the test
            # tools (every graphics backend), and that variant must not be the one published.
            $built = Join-Path $targetRoot "$($entry.Folder)\$ExeName"
            if (-not (Test-Path -LiteralPath $built)) { Stop-Build "cargo did not produce $built" }
            $package = Join-Path $stage "packages\$($entry.Name)"
            [void](New-Item -ItemType Directory -Path $package -Force)
            Copy-Item -LiteralPath $built -Destination (Join-Path $package $ExeName)
        }

        if ($skipTests) {
            Write-Step 'build' 'Tests skipped (--skip-tests)'
        } else {
            Invoke-Step 'build' $cargo @('test', '--locked', '--workspace')
            # The tests that check a download with the Instagram clone's validator pass without
            # Node.js and only print a note, which the captured test output hides: say it here.
            if (-not (Get-Command 'node' -CommandType Application -ErrorAction SilentlyContinue)) {
                Write-Step 'build' 'Note: Node.js is not on PATH, so the tests skipped the checks with the Instagram clone''s export validator (see Tests in README.md).'
            }
        }

        foreach ($name in $names) {
            $package = Join-Path $stage "packages\$name"
            Write-TextFile (Join-Path $package 'README.txt') (Get-PackageReadme $name $version)
            Copy-Item -LiteralPath $licences -Destination (Join-Path $package 'licenses') -Recurse
            Test-Package $package $name $version (Join-Path $stage "smoke\$name")
        }

        Publish-Packages $stage $names
    } catch {
        Write-Step 'build' "Build files retained for troubleshooting until the next build: $stage"
        throw
    }
    try {
        Remove-Item -LiteralPath $stage -Recurse -Force
    } catch {
        Write-Step 'build' "Warning: could not remove all build files in $stage; the next build removes them."
    }

    [Console]::Out.WriteLine('')
    $folders = @($names | ForEach-Object { Join-Path $DistRoot $_ })
    Write-Step 'build' "Ready: $($folders -join ' and ')"
    foreach ($name in $names) {
        $kind = 'optimised, windowed'
        if ($name -eq 'Debug') { $kind = 'console window with the log' }
        $label = ("${name}:").PadRight(8)
        Write-Step 'build' "$label build\dist\$name\$ExeName ($(Format-Size (Join-Path $DistRoot "$name\$ExeName")), $kind)"
    }
    Write-Step 'build' "Open:    $ExeName <picture or folder>, or drag pictures onto the exe"
    Write-Step 'build' "Details: README.txt in each folder; third-party licences in licenses\"
    Write-Step 'build' "Time:    $(Format-Duration ((Get-Date) - $started))"
}
