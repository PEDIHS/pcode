<#
  pcode installer — Pedram's custom build of Jcode Desktop + jcode CLI for Windows.

  Run from a clone / downloaded copy of this repo:
      powershell -ExecutionPolicy Bypass -File install.ps1
  or straight from GitHub (public repo):
      irm https://raw.githubusercontent.com/PEDIHS/pcode/main/install.ps1 | iex

  What it does
    1. Installs prerequisites if missing: Node.js LTS, Python 3.12 (real one, not the Store alias), uv.
    2. Installs the prebuilt custom Jcode Desktop 0.6.5 (Persian/RTL, plugins, team, tray, glass themes)
       and the matching jcode CLI from dist/pcode-windows-x64.zip (SHA-256 verified).
    3. Installs the Windows overlay, the Jcode Browser Agent extension and its MCP launcher
       into %LOCALAPPDATA%\pcode, with a freshly generated bridge token.
    4. Adds jcode to PATH, registers the overlay / windows / browser MCP servers in ~/.jcode/mcp.json
       (only entries that are not there yet), creates Start-menu and Desktop shortcuts.
  It never touches your accounts: sign in inside Jcode after installing.
#>
param([string]$Source)
$ErrorActionPreference = 'Stop'
$Repo   = 'PEDIHS/pcode'
$Branch = 'main'
$AppVer = 'Jcode-0.6.5-windows-x86_64'
function Step($t) { Write-Host ""; Write-Host "==> $t" -ForegroundColor Cyan }
function RefreshPath { $env:Path = [Environment]::GetEnvironmentVariable('Path','Machine') + ';' + [Environment]::GetEnvironmentVariable('Path','User') }
function AddUserPath($dir) {
  $up = [Environment]::GetEnvironmentVariable('Path','User'); if (-not $up) { $up = '' }
  if (($up -split ';') -notcontains $dir) { [Environment]::SetEnvironmentVariable('Path', ($up.TrimEnd(';') + ';' + $dir).TrimStart(';'), 'User'); Write-Host "  PATH += $dir" }
}

RefreshPath

# ---------- source: local repo copy or download from GitHub
if (-not $Source) { $Source = if ($PSScriptRoot) { $PSScriptRoot } else { $null } }
if (-not $Source -or -not (Test-Path (Join-Path $Source 'dist\pcode-windows-x64.zip'))) {
  Step "Downloading pcode from github.com/$Repo"
  $tmp = Join-Path $env:TEMP 'pcode-src'; Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
  New-Item -ItemType Directory -Force $tmp | Out-Null
  $zip = Join-Path $tmp 'src.zip'
  Invoke-WebRequest "https://github.com/$Repo/archive/refs/heads/$Branch.zip" -OutFile $zip -UseBasicParsing
  & tar.exe -xf $zip -C $tmp
  $Source = (Get-ChildItem $tmp -Directory | Select-Object -First 1).FullName
}
Write-Host "Source: $Source"

$Home2   = $env:USERPROFILE
$Pcode   = Join-Path $env:LOCALAPPDATA 'pcode'
$AppRoot = Join-Path $env:LOCALAPPDATA 'Programs\JcodeDesktop'
$Bin     = Join-Path $env:LOCALAPPDATA 'jcode\bin'
$dDrive  = Get-Volume -DriveLetter D -ErrorAction SilentlyContinue
$UvRoot  = if ($dDrive -and $dDrive.DriveType -eq 'Fixed') { 'D:\uv' } else { Join-Path $Home2 'uv' }

Get-Process | Where-Object { $_.Name -match '^jcode' } | Stop-Process -Force -ErrorAction SilentlyContinue

# ---------- prerequisites
Step 'Prerequisites'
$winget = Get-Command winget -ErrorAction SilentlyContinue
function WingetInstall($id, $extra) {
  if (-not $winget) { Write-Warning "  winget missing - install $id manually"; return }
  Write-Host "  installing $id ..."
  & winget install -e --id $id --silent --accept-source-agreements --accept-package-agreements @extra | Out-Null
}
if (-not (Get-Command node -ErrorAction SilentlyContinue)) { WingetInstall 'OpenJS.NodeJS.LTS' @() } else { Write-Host '  node ok' }
$Py = Join-Path $env:LOCALAPPDATA 'Programs\Python\Python312\python.exe'
if (-not (Test-Path $Py)) {
  $real = Get-Command python -ErrorAction SilentlyContinue | Where-Object { $_.Source -notmatch 'WindowsApps' } | Select-Object -First 1
  if ($real) { $Py = $real.Source } else { WingetInstall 'Python.Python.3.12' @('--scope','user') }
}
RefreshPath
if (Test-Path $Py) {
  Write-Host "  python: $Py"
  & $Py -m pip install --user --quiet --disable-pip-version-check uv pillow numpy
  $PyScripts = Join-Path $env:APPDATA 'Python\Python312\Scripts'
  if (Test-Path $PyScripts) { AddUserPath $PyScripts }
} else { Write-Warning '  Python 3.12 not found - overlay / windows MCP need it' }
$Uvx = Join-Path $env:APPDATA 'Python\Python312\Scripts\uvx.exe'

# ---------- app + CLI
Step 'Installing Jcode Desktop (pcode build) and jcode CLI'
$dist = Join-Path $Source 'dist\pcode-windows-x64.zip'
$want = (Get-Content (Join-Path $Source 'dist\pcode-windows-x64.zip.sha256') -Raw).Trim()
$have = (Get-FileHash $dist -Algorithm SHA256).Hash
if ($want -ne $have) { throw "dist zip checksum mismatch (expected $want, got $have)" }
New-Item -ItemType Directory -Force $AppRoot, $Bin | Out-Null
Remove-Item (Join-Path $AppRoot $AppVer) -Recurse -Force -ErrorAction SilentlyContinue
& tar.exe -xf $dist -C $AppRoot
Move-Item (Join-Path $AppRoot "$AppVer\Jcode.ico") (Join-Path $AppRoot 'Jcode.ico') -Force
Copy-Item (Join-Path $AppRoot "$AppVer\jcode.exe") (Join-Path $Bin 'jcode.exe') -Force
AddUserPath $Bin
Write-Host "  ok  $AppRoot\$AppVer"

# ---------- overlay, browser extension, browser MCP
Step 'Installing overlay, browser extension and browser MCP'
New-Item -ItemType Directory -Force $Pcode | Out-Null
foreach ($d in 'overlay','browser-extension','browser-mcp') {
  $to = Join-Path $Pcode $d; New-Item -ItemType Directory -Force $to | Out-Null
  Copy-Item (Join-Path $Source "$d\*") $to -Recurse -Force
}
$ExtDir = Join-Path $Pcode 'browser-extension'
$cfg = Join-Path $ExtDir 'config.json'
if (-not (Test-Path $cfg)) {
  $b = New-Object byte[] 32; [Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($b)
  $tok = [Convert]::ToBase64String($b).Replace('+','-').Replace('/','_').TrimEnd('=')
  [IO.File]::WriteAllText($cfg, "{`"token`": `"$tok`"}", (New-Object Text.UTF8Encoding($false)))
  Write-Host '  generated new browser bridge token'
}
RefreshPath
if (Get-Command npm -ErrorAction SilentlyContinue) {
  Push-Location (Join-Path $Pcode 'browser-mcp'); & npm install --silent --no-audit --no-fund | Out-Null; Pop-Location
  Write-Host '  browser-mcp npm install ok'
} else { Write-Warning '  npm not found - run "npm install" in browser-mcp later' }

# ---------- MCP config (add only missing entries)
Step 'Registering MCP servers in ~/.jcode/mcp.json'
$jh = Join-Path $Home2 '.jcode'; New-Item -ItemType Directory -Force $jh | Out-Null
$mcpPath = Join-Path $jh 'mcp.json'
$mcp = if (Test-Path $mcpPath) { Get-Content $mcpPath -Raw | ConvertFrom-Json } else { [pscustomobject]@{ servers = [pscustomobject]@{} } }
$key = if ($mcp.PSObject.Properties.Name -contains 'mcpServers') { 'mcpServers' } else { 'servers' }
if (-not $mcp.$key) { $mcp | Add-Member -NotePropertyName $key -NotePropertyValue ([pscustomobject]@{}) -Force }
$uvEnv = [pscustomobject]@{ UV_PYTHON_INSTALL_DIR = "$UvRoot\python"; UV_CACHE_DIR = "$UvRoot\cache" }
$entries = [ordered]@{
  'windows' = [pscustomobject]@{ command = $Uvx; args = @('--python','3.13','windows-mcp','serve','--transport','stdio'); env = $uvEnv; shared = $true; enabled = $true }
  'windows-overlay' = [pscustomobject]@{ command = $Py; args = @((Join-Path $Pcode 'overlay\proxy.py'),'--',$Uvx,'--python','3.13','windows-mcp','serve','--transport','stdio'); env = [pscustomobject]@{ UV_PYTHON_INSTALL_DIR = "$UvRoot\python"; UV_CACHE_DIR = "$UvRoot\cache"; JCODE_OVERLAY_LOG = (Join-Path $Pcode 'overlay\overlay.log') }; shared = $true; enabled = $true }
  'jcode-browser' = [pscustomobject]@{ command = (Get-Command node -ErrorAction SilentlyContinue).Source; args = @((Join-Path $Pcode 'browser-mcp\launch.cjs')); env = [pscustomobject]@{ JCODE_EXTENSION_DIR = $ExtDir }; shared = $true; enabled = $false }
}
$added = 0
foreach ($n in $entries.Keys) { if (-not ($mcp.$key.PSObject.Properties.Name -contains $n)) { $mcp.$key | Add-Member -NotePropertyName $n -NotePropertyValue $entries[$n]; $added++; Write-Host "  + $n" } }
if ($added) { [IO.File]::WriteAllText($mcpPath, ($mcp | ConvertTo-Json -Depth 10), (New-Object Text.UTF8Encoding($false))) } else { Write-Host '  all entries already present' }

# ---------- shortcuts
Step 'Creating shortcuts'
$exe = Join-Path $AppRoot "$AppVer\jcode-desktop.exe"
$ws = New-Object -ComObject WScript.Shell
foreach ($lnk in @((Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Jcode.lnk'), (Join-Path ([Environment]::GetFolderPath('Desktop')) 'Jcode.lnk'))) {
  $s = $ws.CreateShortcut($lnk); $s.TargetPath = $exe; $s.WorkingDirectory = (Split-Path $exe); $s.IconLocation = (Join-Path $AppRoot 'Jcode.ico'); $s.Save(); Write-Host "  $lnk"
}

Step 'Check'
& (Join-Path $Bin 'jcode.exe') --version
if (Test-Path $exe) { Write-Host '  jcode-desktop.exe OK' } else { Write-Warning '  jcode-desktop.exe missing' }
Write-Host ""
Write-Host 'DONE.' -ForegroundColor Green
Write-Host '  1) Open Jcode from the Desktop and sign in to your providers.'
Write-Host "  2) Browser agent: chrome://extensions > Developer mode > Load unpacked > $ExtDir"
Write-Host '  3) Open a NEW terminal so PATH is refreshed, then run: jcode'
