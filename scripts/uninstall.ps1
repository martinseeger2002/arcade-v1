# Arcade V1.0 uninstaller for Windows. Removes the programs, the login task, the Start
# menu shortcut, and your data (wallet, its 12 words, messages, chains).
#
#   powershell -NoProfile -ExecutionPolicy Bypass -Command "[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; iex ((New-Object Net.WebClient).DownloadString('https://raw.githubusercontent.com/martinseeger2002/arcade-v1/main/scripts/uninstall.ps1'))"
#
# It asks you to type DELETE first. Without a way to ask, it removes nothing unless
# ARCADE_UNINSTALL_YES=1 is set. --keep-data removes only the programs, the login task and the
# shortcuts, asks nothing, and leaves the data folder (wallet, 12 words, chains) where it is: the
# Setup.exe's uninstaller (Settings > Apps) runs it that way. ARCADE_HOME, when set, is the data folder; otherwise the
# folder recorded at install time (%LOCALAPPDATA%\Programs\Arcade\home.txt), otherwise
# %LOCALAPPDATA%\arcade.

$ErrorActionPreference = 'Stop'
$TaskName = 'Arcade V1.0'
$KeepData = (@($args) -contains '--keep-data')

function Say([string]$Msg) { Write-Host $Msg -ForegroundColor Cyan }

function LocalAppData {
    if ($env:LOCALAPPDATA) { return $env:LOCALAPPDATA }
    return (Join-Path $env:USERPROFILE 'AppData\Local')
}

$local = LocalAppData
$bin = Join-Path $local 'Programs\Arcade'
$recorded = Join-Path $bin 'home.txt'
$DataDir = $null
if ($env:ARCADE_HOME) {
    $DataDir = $env:ARCADE_HOME
} elseif (Test-Path -LiteralPath $recorded) {
    $DataDir = (Get-Content -LiteralPath $recorded -TotalCount 1).Trim()
} else {
    $DataDir = Join-Path $local 'arcade'
}

function Test-SafeToDelete([string]$Path) {
    if (-not $Path) { return $false }
    $full = [IO.Path]::GetFullPath($Path).TrimEnd('\')
    if ($full.Length -lt 4) { return $false }
    $bad = @(
        $env:USERPROFILE,
        $env:LOCALAPPDATA,
        $env:APPDATA,
        $env:SystemRoot,
        $env:ProgramFiles,
        ${env:ProgramFiles(x86)},
        (Join-Path $env:SystemDrive '\'),
        'C:\',
        'C:\Windows'
    )
    foreach ($b in $bad) {
        if (-not $b) { continue }
        $n = $b.TrimEnd('\')
        if ($n -and ($full -eq $n)) { return $false }
    }
    return $true
}

if (-not $KeepData -and -not (Test-SafeToDelete $DataDir)) {
    Write-Host "Refusing: the data folder would be $DataDir."
    exit 1
}

$size = ''
if ($KeepData) {
    Say "Removing the Arcade V1.0 programs. Your data stays in ${DataDir}."
} elseif (Test-Path -LiteralPath $DataDir) {
    $sum = (Get-ChildItem -LiteralPath $DataDir -Recurse -Force -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
    if ($sum) { $size = (' ({0:N0} MB)' -f ($sum / 1MB)) }
}

if (-not $KeepData) {
Say "This removes Arcade V1.0 completely from this computer:"
Write-Host "  - the programs, the Start menu shortcut, and the login task"
Write-Host "  - your data in ${DataDir}${size}:"
Write-Host "    your wallet and its 12 words, imported wallets, messages, groups and the chains"
Write-Host ""
if (Test-Path (Join-Path $DataDir 'arcade.mdbx')) {
    $arcade = Join-Path $bin 'arcade.exe'
    Say "If your wallet holds anything (coins, your @handle), write down your 12 words first:"
    Write-Host "  & `"$arcade`" --home `"$DataDir`" wallet phrase"
    Write-Host "Without them the wallet can't be restored once this is done."
    Write-Host ""
}

}

$answer = ''
if ($KeepData -or $env:ARCADE_UNINSTALL_YES -eq '1') {
    $answer = 'DELETE'
} else {
    try {
        $answer = Read-Host 'Type DELETE to remove everything (anything else cancels)'
    } catch {
        Write-Host "No terminal to ask: nothing removed. (ARCADE_UNINSTALL_YES=1 removes without asking.)"
        exit 1
    }
}
if ($answer -ne 'DELETE') {
    Write-Host "Cancelled: nothing removed."
    exit 0
}

Say "Removing Arcade V1.0..."
Disable-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue | Out-Null
if ((Test-Path -LiteralPath $DataDir) -and (Get-Process -Name 'arcaded' -ErrorAction SilentlyContinue)) {
    Say "Waiting for arcaded to finish writing (up to 5 minutes)..."
    Set-Content -LiteralPath (Join-Path $DataDir 'arcaded.stop') -Value 'stop' -Encoding ascii
    $deadline = (Get-Date).AddSeconds(300)
    while ((Get-Process -Name 'arcaded' -ErrorAction SilentlyContinue) -and (Get-Date) -lt $deadline) {
        Start-Sleep -Seconds 1
    }
}
Stop-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false -ErrorAction SilentlyContinue
# The installer's firewall rule (added only when it ran as administrator; removing it needs one too).
try { Remove-NetFirewallRule -DisplayName 'Arcade V1.0 (arcaded)' -ErrorAction SilentlyContinue } catch {}
Get-Process -Name 'arcaded','arcade-signer','arcade-desktop' -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue

$startup = Join-Path ([Environment]::GetFolderPath('Startup')) 'Arcade V1.0.lnk'
$menu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Arcade V1.0.lnk'
$desktop = Join-Path ([Environment]::GetFolderPath('Desktop')) 'Arcade V1.0.lnk'
Remove-Item -LiteralPath $startup, $menu, $desktop -Force -ErrorAction SilentlyContinue

$cur = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($cur) {
    $norm = $bin.TrimEnd('\')
    $kept = @()
    foreach ($p in ($cur -split ';')) {
        if ($p -and ($p.TrimEnd('\') -ne $norm)) { $kept += $p }
    }
    [Environment]::SetEnvironmentVariable('Path', ($kept -join ';'), 'User')
}

if (Test-Path -LiteralPath $bin) { Remove-Item -LiteralPath $bin -Recurse -Force }
if ($KeepData) {
    Say "Arcade V1.0's programs are removed. Your wallet, its 12 words and your chains are still in ${DataDir}: installing again picks them up."
    exit 0
}
if (Test-Path -LiteralPath $DataDir) { Remove-Item -LiteralPath $DataDir -Recurse -Force }
foreach ($extra in @('arcade-v1-src', 'arcade-v1-cache')) {
    $p = Join-Path $local $extra
    if (Test-Path -LiteralPath $p) { Remove-Item -LiteralPath $p -Recurse -Force }
}

Say "Arcade V1.0 is removed, with its data. Installing again starts fresh (restore a wallet from its 12 words then)."
exit 0
