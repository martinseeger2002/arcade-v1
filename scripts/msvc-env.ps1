# Environment for building Arcade on Windows (MSVC).
#
# libmdbx's crate runs bindgen, which needs libclang and the MSVC headers. This script
# finds both and exports LIBCLANG_PATH, BINDGEN_EXTRA_CLANG_ARGS and CFLAGS.
#
# Dot-source it (the variables have to land in this session):
#
#   . .\scripts\msvc-env.ps1
#   cargo build --release --locked -p arcaded -p arcade-cli -p arcade-signer -p arcade-desktop
#
# Or let it run a command in that environment:
#
#   powershell -File scripts\msvc-env.ps1 -- cargo build --release
#
# Linux builds do not use this file.

$DotSource = ($MyInvocation.InvocationName -eq '.')
$savedErrorAction = $ErrorActionPreference
$ErrorActionPreference = 'Stop'

function Add-EnvPath([string]$Dir) {
    if (-not $Dir -or -not (Test-Path $Dir)) { return }
    $norm = $Dir.TrimEnd('\')
    foreach ($p in ($env:Path -split ';')) {
        if ($p.TrimEnd('\') -eq $norm) { return }
    }
    $env:Path = "$Dir;$env:Path"
}

function Publish-Env([string]$Name, [string]$Value) {
    Set-Item -Path "Env:$Name" -Value $Value
    if ($env:GITHUB_ENV) {
        Add-Content -Path $env:GITHUB_ENV -Value "$Name=$Value"
    }
}

function Import-VcVars {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path $vswhere)) {
        throw "Visual Studio is not installed (vswhere.exe is missing). Install the Build Tools with the `"Desktop development with C++`" workload: https://visualstudio.microsoft.com/visual-cpp-build-tools/"
    }
    $vs = (& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath)
    if ($vs) { $vs = $vs.ToString().Trim() }
    if (-not $vs) {
        throw "The Visual Studio C++ toolset is not installed. Install the `"Desktop development with C++`" workload and run this again."
    }
    $bat = Join-Path $vs 'VC\Auxiliary\Build\vcvars64.bat'
    if (-not (Test-Path $bat)) {
        throw "vcvars64.bat was not found at $bat"
    }
    $lines = cmd /c "`"$bat`" >nul && set"
    foreach ($line in $lines) {
        if ($line -match '^([^=]+)=(.*)$') {
            Set-Item -Path "Env:$($Matches[1])" -Value $Matches[2]
        }
    }
    if (-not $env:INCLUDE) {
        throw "vcvars64.bat did not set INCLUDE, so libclang cannot see the Windows headers."
    }
}

function Find-LlvmBin {
    $candidates = @()
    if ($env:LIBCLANG_PATH) { $candidates += $env:LIBCLANG_PATH }
    if ($env:LLVM_PATH) { $candidates += (Join-Path $env:LLVM_PATH 'bin') }
    $candidates += (Join-Path $env:ProgramFiles 'LLVM\bin')
    $candidates += (Join-Path ${env:ProgramFiles(x86)} 'LLVM\bin')
    $clang = Get-Command clang -ErrorAction SilentlyContinue
    if ($clang) { $candidates += (Split-Path -Parent $clang.Source) }
    foreach ($c in $candidates) {
        if ($c -and (Test-Path (Join-Path $c 'libclang.dll'))) { return $c }
    }
    return $null
}

try {
    Import-VcVars
    $llvm = Find-LlvmBin
    if (-not $llvm) {
        throw "LLVM's libclang.dll was not found. Install LLVM (winget install LLVM.LLVM) and run this again. libmdbx needs it to generate its bindings."
    }
    Add-EnvPath $llvm
    Publish-Env 'LIBCLANG_PATH' $llvm

    # bindgen splits this with a shell lexer, and a backslash is an escape, so include
    # paths are forward-slash quoted strings.
    $clangArgs = @(
        '-D_WIN32',
        '-D_WIN64',
        '-D_Check_return_=',
        '--target=x86_64-pc-windows-msvc',
        '-fms-compatibility',
        '-fms-extensions',
        '-fms-compatibility-version=19.33'
    )
    foreach ($inc in ($env:INCLUDE -split ';')) {
        if (-not $inc) { continue }
        $fwd = ($inc.Trim().Replace('\', '/'))
        $clangArgs += "-isystem`"$fwd`""
    }
    if ($env:BINDGEN_EXTRA_CLANG_ARGS -and $env:BINDGEN_EXTRA_CLANG_ARGS -notmatch '_WIN32') {
        $clangArgs += $env:BINDGEN_EXTRA_CLANG_ARGS
    }
    Publish-Env 'BINDGEN_EXTRA_CLANG_ARGS' ($clangArgs -join ' ')

    if ($env:CFLAGS -and $env:CFLAGS -notmatch '/utf-8') {
        Publish-Env 'CFLAGS' ($env:CFLAGS + ' /utf-8')
    } elseif (-not $env:CFLAGS) {
        Publish-Env 'CFLAGS' '/utf-8'
    }

    if ($env:GITHUB_ENV) {
        # Later steps of a GitHub job do not see this process. PATH and INCLUDE have to travel.
        Add-Content -Path $env:GITHUB_ENV -Value "PATH=$env:Path"
        Publish-Env 'INCLUDE' $env:INCLUDE
        if ($env:LIB) { Publish-Env 'LIB' $env:LIB }
        if ($env:LIBPATH) { Publish-Env 'LIBPATH' $env:LIBPATH }
    }

    $dash = [array]::IndexOf($args, '--')
    if (-not $DotSource -and $dash -ge 0 -and $dash -lt ($args.Count - 1)) {
        $cmd = @($args[($dash + 1)..($args.Count - 1)])
        & $cmd[0] @($cmd | Select-Object -Skip 1)
        exit $LASTEXITCODE
    }
} finally {
    if ($DotSource) { $ErrorActionPreference = $savedErrorAction }
}
