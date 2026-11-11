# Arcade V1.0 on Windows: build from this clone and install for the current user.
#
#   powershell -ExecutionPolicy Bypass -File scripts\install-windows.ps1 [-NoDesktop] [-NoBuild] [-NoStart]
#
# Needs Rust (rustup, MSVC toolchain) and LLVM (for libmdbx's bindings; winget install LLVM.LLVM).
# Installs, with no administrator rights:
#   - arcaded.exe, arcade.exe, arcade-desktop.exe  -> %LOCALAPPDATA%\Programs\Arcade (added to your PATH)
#   - the service started now and at every logon, hidden (Startup folder: "Arcade service.vbs"),
#     logging to %USERPROFILE%\.arcade\arcaded.log
#   - a Start menu entry "Arcade V1.0" for the desktop app
# Data stays in %USERPROFILE%\.arcade (wallet, 12 words, @handle, chains). Run it again to update:
# the running service is stopped cleanly first (`arcade stop` writes its caches), then restarted.

param([switch]$NoDesktop, [switch]$NoBuild, [switch]$NoStart)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $env:LOCALAPPDATA 'Programs\Arcade'
$home_ = Join-Path $env:USERPROFILE '.arcade'

if (-not $NoBuild) {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { throw 'cargo not found: install Rust from https://rustup.rs' }
    if (-not $env:LIBCLANG_PATH) {
        foreach ($d in @("$env:ProgramFiles\LLVM\bin", "${env:ProgramFiles(x86)}\LLVM\bin")) {
            if (Test-Path (Join-Path $d 'libclang.dll')) { $env:LIBCLANG_PATH = $d; break }
        }
    }
    if (-not $env:LIBCLANG_PATH) { throw 'libclang.dll not found: install LLVM (winget install LLVM.LLVM) or set LIBCLANG_PATH' }
    $pkgs = @('-p', 'arcaded', '-p', 'arcade-cli')
    if (-not $NoDesktop) { $pkgs += @('-p', 'arcade-desktop') }
    Write-Host 'Building (the first build takes several minutes)...'
    & cargo build --release --manifest-path (Join-Path $repo 'Cargo.toml') @pkgs
    if ($LASTEXITCODE -ne 0) { throw 'build failed' }
}

# Stop a running arcaded cleanly so its .exe can be replaced.
$cli = Join-Path $dest 'arcade.exe'
if ((Get-Process arcaded -ErrorAction SilentlyContinue) -and (Test-Path $cli)) {
    Write-Host 'Stopping the running arcaded (writing its caches)...'
    & $cli stop | Out-Null
    $t = 0
    while ((Get-Process arcaded -ErrorAction SilentlyContinue) -and $t -lt 300) { Start-Sleep 1; $t++ }
}
if (Get-Process arcaded -ErrorAction SilentlyContinue) { throw 'arcaded is still running; stop it (arcade stop) and run this again' }
Get-Process arcade-desktop -ErrorAction SilentlyContinue | Stop-Process

New-Item -ItemType Directory -Force $dest | Out-Null
$bins = @('arcaded.exe', 'arcade.exe')
if (-not $NoDesktop) { $bins += 'arcade-desktop.exe' }
foreach ($b in $bins) { Copy-Item (Join-Path $repo "target\release\$b") $dest -Force }
Write-Host "Installed $($bins -join ', ') to $dest"

# Start at logon, hidden, logging to the arcade's folder.
New-Item -ItemType Directory -Force $home_ | Out-Null
$startup = [Environment]::GetFolderPath('Startup')
$vbs = Join-Path $startup 'Arcade service.vbs'
$cmdline = 'cmd /c ""' + (Join-Path $dest 'arcaded.exe') + '" >> "' + (Join-Path $home_ 'arcaded.log') + '" 2>&1"'
Set-Content -Path $vbs -Encoding ASCII -Value @(
    "' Arcade V1.0: start arcaded hidden at logon (scripts\install-windows.ps1).",
    ('CreateObject("WScript.Shell").Run "' + $cmdline.Replace('"', '""') + '", 0, False')
)
Write-Host "Starts at logon: $vbs"

if (-not $NoDesktop) {
    $menu = Join-Path ([Environment]::GetFolderPath('Programs')) 'Arcade V1.0.lnk'
    $sc = (New-Object -ComObject WScript.Shell).CreateShortcut($menu)
    $sc.TargetPath = Join-Path $dest 'arcade-desktop.exe'
    $sc.WorkingDirectory = $dest
    $sc.Save()
    Write-Host "Start menu: $menu"
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not (($userPath -split ';') -contains $dest)) {
    [Environment]::SetEnvironmentVariable('Path', ($userPath.TrimEnd(';') + ";$dest").TrimStart(';'), 'User')
    Write-Host "Added $dest to your PATH (open a new terminal for it)"
}

if (-not $NoStart) {
    Start-Process wscript.exe -ArgumentList "`"$vbs`""
    Start-Sleep 5
    if (Get-Process arcaded -ErrorAction SilentlyContinue) { Write-Host 'arcaded is running. Try: arcade status' }
    else { Write-Warning "arcaded did not start; see $(Join-Path $home_ 'arcaded.log')" }
}
