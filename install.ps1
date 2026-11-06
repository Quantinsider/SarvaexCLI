# Installs the sarvaex CLI on Windows:
#   irm https://raw.githubusercontent.com/Rustix69/SarvaexCLI/main/install.ps1 | iex
# Pin a version with $env:SARVAEX_VERSION = "0.1.0".
$ErrorActionPreference = "Stop"
$repo = "Rustix69/SarvaexCLI"
$version = $env:SARVAEX_VERSION
if (-not $version) {
  $version = (Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest").tag_name.TrimStart("v")
}
$name = "sarvaex-$version-x86_64-pc-windows-msvc"
$base = "https://github.com/$repo/releases/download/v$version"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("sarvaex-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
  Write-Host "Downloading sarvaex $version ..."
  Invoke-WebRequest "$base/$name.zip" -OutFile "$tmp\$name.zip"
  try {
    $expect = ((Invoke-WebRequest "$base/$name.zip.sha256").Content -split "\s+")[0]
    $got = (Get-FileHash "$tmp\$name.zip" -Algorithm SHA256).Hash
    if ($got -ne $expect) { throw "checksum mismatch, not installing" }
    Write-Host "Checksum OK"
  } catch [System.Net.WebException] { }
  Expand-Archive "$tmp\$name.zip" -DestinationPath $tmp
  $dest = Join-Path $env:LOCALAPPDATA "sarvaex\bin"
  New-Item -ItemType Directory -Force -Path $dest | Out-Null
  Copy-Item "$tmp\$name\sarvaex.exe" "$dest\sarvaex.exe" -Force
  $path = [Environment]::GetEnvironmentVariable("Path", "User")
  if ($path -notlike "*$dest*") { [Environment]::SetEnvironmentVariable("Path", "$path;$dest", "User"); Write-Host "Added $dest to your PATH. Open a new terminal." }
  Write-Host "Installed $dest\sarvaex.exe"
  Write-Host "Next:  sarvaex auth login"
} finally { Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue }
