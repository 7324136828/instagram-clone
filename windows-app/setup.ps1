# Checks the tools this project is built with and downloads the crates it depends on.
#
# setup.bat runs this script. The project needs exactly two things on a Windows machine:
#
#   * Rust, the stable toolchain for x86_64-pc-windows-msvc, from https://rustup.rs
#   * the Visual Studio C++ build tools (the linker and the C runtime libraries) with a
#     Windows SDK: the workload "Desktop development with C++" of Visual Studio or of the
#     Build Tools for Visual Studio
#
# Nothing else: no Python, no Node.js, no CMake, no NASM, no Clang. The script names what is
# missing and where to get it, then runs "cargo fetch --locked".
#
# build.ps1 loads this file for the same checks: everything above the last block is a function
# or a constant, and the last block only runs when the script is started, not when it is loaded.
#
# The file is plain ASCII on purpose: Windows PowerShell 5.1 reads a script without a byte
# order mark in the system's code page.

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

# The project folder: the folder of this script.
$ProjectRoot = $PSScriptRoot
# Everything a build writes stays in here (.cargo/config.toml puts cargo's target folder in it).
$BuildRoot = Join-Path $ProjectRoot 'build'
# Written when setup has passed; run.bat runs setup first while it is missing.
$SetupStamp = Join-Path $BuildRoot 'setup.stamp'
# The only target this project is built for.
$RequiredHost = 'x86_64-pc-windows-msvc'
# The Visual Studio component that holds the C++ compiler, the linker and the C runtime.
$VcToolsComponent = 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64'

$RustAdvice = 'Install Rust from https://rustup.rs and keep the default toolchain (stable-x86_64-pc-windows-msvc).'
$VisualStudioAdvice = 'Install the Build Tools for Visual Studio (or Visual Studio) from https://visualstudio.microsoft.com/downloads/ and select the workload "Desktop development with C++". It brings the component "MSVC x64/x86 build tools" and a Windows SDK.'
$SdkAdvice = 'Open the Visual Studio Installer, choose Modify, and under "Desktop development with C++" select a Windows 11 SDK (a Windows 10 SDK works as well).'

# Prints one line of this script's own output, tagged with the name of the script.
function Write-Step {
    param([string]$Tag, [string]$Text)
    [Console]::Out.WriteLine("[$Tag] $Text")
}

# One argument as the Microsoft C runtime (and so cargo and the app) parses it back.
function Format-Argument {
    param([string]$Argument)
    if ($Argument -ne '' -and $Argument -notmatch '[\s"]') {
        return $Argument
    }
    # Backslashes are literal unless they stand before a quote; those are doubled.
    $escaped = [regex]::Replace($Argument, '(\\*)"', '$1$1\"')
    $escaped = [regex]::Replace($escaped, '(\\+)$', '$1$1')
    return '"' + $escaped + '"'
}

# A command line as it is echoed and started.
function Format-CommandLine {
    param([string[]]$Arguments)
    return (($Arguments | ForEach-Object { Format-Argument $_ }) -join ' ')
}

# Starts a program and waits for it. Its output goes straight to this console (or to wherever
# the output of the script is redirected), untouched: Windows PowerShell would turn every line
# a program writes to its error stream into an error record, and cargo writes its progress
# there. With -Capture the standard output is returned as text instead; with -Quiet both
# streams are collected and returned, for a program whose chatter is only of interest when it
# fails. Returns an object with ExitCode and Output.
function Start-Tool {
    param(
        [string]$FilePath,
        [string[]]$Arguments = @(),
        [switch]$Capture,
        [switch]$Quiet,
        [int]$TimeoutSeconds = 0
    )
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $FilePath
    $info.Arguments = Format-CommandLine $Arguments
    $info.WorkingDirectory = $ProjectRoot
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $false
    if ($Capture -or $Quiet) {
        $info.RedirectStandardOutput = $true
        $info.StandardOutputEncoding = [System.Text.Encoding]::UTF8
    }
    if ($Quiet) {
        $info.RedirectStandardError = $true
        $info.StandardErrorEncoding = [System.Text.Encoding]::UTF8
    }
    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $info
    [void]$process.Start()
    $output = ''
    $errors = ''
    $outputTask = $null
    $errorTask = $null
    # Both streams are read while the program runs, so that it never waits for a full pipe.
    if ($Capture -or $Quiet) { $outputTask = $process.StandardOutput.ReadToEndAsync() }
    if ($Quiet) { $errorTask = $process.StandardError.ReadToEndAsync() }
    $timedOut = $false
    if ($TimeoutSeconds -gt 0) {
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            $timedOut = $true
            try { $process.Kill() } catch { }
        }
    }
    $process.WaitForExit()
    if ($outputTask) { $output = $outputTask.Result }
    if ($errorTask) { $errors = $errorTask.Result }
    $exitCode = $process.ExitCode
    $process.Dispose()
    return New-Object PSObject -Property @{ ExitCode = $exitCode; Output = $output; Errors = $errors; TimedOut = $timedOut }
}

# Echoes a command as "[tag] $ command", runs it with its output passed through, and stops the
# script when it fails.
function Invoke-Step {
    param([string]$Tag, [string]$FilePath, [string[]]$Arguments = @())
    $name = [System.IO.Path]::GetFileNameWithoutExtension($FilePath)
    Write-Step $Tag ('$ ' + (Format-CommandLine (@($name) + $Arguments)))
    $result = Start-Tool -FilePath $FilePath -Arguments $Arguments
    if ($result.ExitCode -ne 0) {
        throw "$name $($Arguments[0]) failed with exit code $($result.ExitCode)"
    }
}

# The full path of cargo.exe. A terminal opened before Rust was installed does not have
# rustup's folder on its PATH yet, so that folder is looked into as well and then put on the
# PATH of this run.
function Find-Cargo {
    param([string]$Tag)
    $command = Get-Command 'cargo.exe' -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($command) {
        return $command.Source
    }
    $cargoHome = $env:CARGO_HOME
    if (-not $cargoHome) { $cargoHome = Join-Path $env:USERPROFILE '.cargo' }
    $candidate = Join-Path (Join-Path $cargoHome 'bin') 'cargo.exe'
    if (Test-Path -LiteralPath $candidate) {
        $env:PATH = (Split-Path -Parent $candidate) + ';' + $env:PATH
        Write-Step $Tag "cargo is not on PATH; using $candidate"
        return $candidate
    }
    throw "Rust was not found: cargo.exe is neither on PATH nor in $(Split-Path -Parent $candidate). $RustAdvice"
}

# Checks that rustc builds for 64-bit Windows with the MSVC linker and returns its version line.
function Test-RustToolchain {
    param([string]$Tag, [string]$Cargo)
    $rustc = Join-Path (Split-Path -Parent $Cargo) 'rustc.exe'
    if (-not (Test-Path -LiteralPath $rustc)) {
        throw "rustc.exe is missing beside $Cargo. $RustAdvice"
    }
    $result = Start-Tool -FilePath $rustc -Arguments @('-vV') -Quiet
    if ($result.ExitCode -ne 0) {
        throw "rustc could not be started: $(($result.Errors + $result.Output).Trim()) $RustAdvice"
    }
    $lines = $result.Output -split "`r?`n"
    $version = $lines[0].Trim()
    $hostLine = $lines | Where-Object { $_ -match '^host:\s*(\S+)' } | Select-Object -First 1
    $hostTriple = ''
    if ($hostLine -and $hostLine -match '^host:\s*(\S+)') { $hostTriple = $Matches[1] }
    if ($hostTriple -ne $RequiredHost) {
        throw "The active Rust toolchain builds for `"$hostTriple`", but this project needs $RequiredHost. Run: rustup default stable-$RequiredHost"
    }
    if ($version -match '-(nightly|beta)') {
        Write-Step $Tag "Warning: $version is not a stable toolchain; the project is developed with stable."
    }
    return $version
}

# Finds the Visual Studio C++ tools with vswhere and returns the installation folder, the
# version of the tools and the path of link.exe.
function Find-VisualStudioTools {
    $programFiles = ${env:ProgramFiles(x86)}
    if (-not $programFiles) { $programFiles = $env:ProgramFiles }
    $vswhere = Join-Path $programFiles 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere)) {
        throw "The Visual Studio C++ build tools were not found (there is no $vswhere). $VisualStudioAdvice"
    }
    $result = Start-Tool -FilePath $vswhere -Arguments @('-latest', '-products', '*', '-requires', $VcToolsComponent, '-property', 'installationPath', '-utf8') -Quiet
    $installation = ($result.Output -split "`r?`n" | Where-Object { $_.Trim() -ne '' } | Select-Object -First 1)
    if ($result.ExitCode -ne 0 -or -not $installation) {
        throw "No Visual Studio installation has the C++ build tools (component $VcToolsComponent). $VisualStudioAdvice"
    }
    $installation = $installation.Trim()
    $versionFile = Join-Path $installation 'VC\Auxiliary\Build\Microsoft.VCToolsVersion.default.txt'
    if (-not (Test-Path -LiteralPath $versionFile)) {
        throw "The C++ build tools in $installation are incomplete ($versionFile is missing). Repair the installation in the Visual Studio Installer."
    }
    $toolsVersion = (Get-Content -LiteralPath $versionFile -TotalCount 1).Trim()
    $bin = Join-Path $installation "VC\Tools\MSVC\$toolsVersion\bin\Hostx64\x64"
    $linker = Join-Path $bin 'link.exe'
    if (-not (Test-Path -LiteralPath $linker)) {
        throw "The 64-bit linker is missing ($linker). Repair the C++ build tools in the Visual Studio Installer."
    }
    return New-Object PSObject -Property @{ Installation = $installation; Version = $toolsVersion; Bin = $bin; Linker = $linker }
}

# Finds a Windows SDK that has the 64-bit import libraries and the resource compiler (the icon
# and the version information of the exe are compiled with rc.exe). Returns its version.
function Find-WindowsSdk {
    $roots = @()
    foreach ($key in @('HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows Kits\Installed Roots', 'HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots')) {
        $value = $null
        try { $value = (Get-ItemProperty -LiteralPath $key -Name 'KitsRoot10' -ErrorAction Stop).KitsRoot10 } catch { }
        if ($value) { $roots += $value }
    }
    foreach ($root in ($roots | Select-Object -Unique)) {
        $libraries = Join-Path $root 'Lib'
        if (-not (Test-Path -LiteralPath $libraries)) { continue }
        $versions = Get-ChildItem -LiteralPath $libraries -Directory | Where-Object { $_.Name -match '^\d+\.\d+\.\d+\.\d+$' } | Sort-Object { [version]$_.Name } -Descending
        foreach ($version in $versions) {
            $library = Join-Path $version.FullName 'um\x64\kernel32.lib'
            $compiler = Join-Path $root "bin\$($version.Name)\x64\rc.exe"
            if ((Test-Path -LiteralPath $library) -and (Test-Path -LiteralPath $compiler)) {
                return $version.Name
            }
        }
    }
    throw "A Windows SDK with the 64-bit libraries and rc.exe was not found. $SdkAdvice"
}

# The pre-flight checks of setup and build: Rust, the C++ tools, a Windows SDK. Prints what it
# found and returns it; stops with a message that says what to install when something is missing.
function Test-Toolchain {
    param([string]$Tag)
    if ($env:OS -ne 'Windows_NT') {
        throw 'This project builds a Windows program; run the script on Windows.'
    }
    $cargo = Find-Cargo $Tag
    $rust = Test-RustToolchain $Tag $cargo
    Write-Step $Tag "Rust:      $rust ($RequiredHost) in $(Split-Path -Parent $cargo)"
    $tools = Find-VisualStudioTools
    Write-Step $Tag "C++ tools: MSVC $($tools.Version) in $($tools.Installation)"
    $sdk = Find-WindowsSdk
    Write-Step $Tag "SDK:       Windows SDK $sdk"
    return New-Object PSObject -Property @{ Cargo = $cargo; Rust = $rust; Tools = $tools; Sdk = $sdk }
}

# Runs main and turns a failure into "[tag] ERROR: message" on the error stream and exit code 1.
function Invoke-Main {
    param([string]$Tag, [scriptblock]$Main)
    try {
        & $Main
        exit 0
    } catch {
        [Console]::Out.Flush()
        [Console]::Error.WriteLine('')
        [Console]::Error.WriteLine("[$Tag] ERROR: $($_.Exception.Message)")
        exit 1
    }
}

# Loaded by build.ps1 (". setup.ps1"): only the functions above are wanted.
if ($MyInvocation.InvocationName -eq '.') {
    return
}

$SetupArguments = @($args)

Invoke-Main 'setup' {
    if ($SetupArguments.Count -gt 0) {
        throw "setup takes no options, but got `"$($SetupArguments[0])`"."
    }
    $toolchain = Test-Toolchain 'setup'
    Write-Step 'setup' 'Downloading the crates named in Cargo.lock'
    Invoke-Step 'setup' $toolchain.Cargo @('fetch', '--locked')
    if (-not (Test-Path -LiteralPath $BuildRoot)) {
        [void](New-Item -ItemType Directory -Path $BuildRoot)
    }
    $stamp = "$($toolchain.Rust)`r`nMSVC $($toolchain.Tools.Version)`r`nWindows SDK $($toolchain.Sdk)`r`n"
    [System.IO.File]::WriteAllText($SetupStamp, $stamp, (New-Object System.Text.ASCIIEncoding))
    [Console]::Out.WriteLine('')
    Write-Step 'setup' 'Ready. Start the application with run.bat, or build it with build.bat'
}
