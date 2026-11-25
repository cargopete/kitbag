# Install kitbag on Windows from GitHub Releases.
#   irm https://raw.githubusercontent.com/cargopete/kitbag/main/install.ps1 | iex
# Environment: KITBAG_VERSION (default latest), KITBAG_INSTALL_DIR (default %LOCALAPPDATA%\kitbag\bin)
$ErrorActionPreference = 'Stop'
$repo = 'cargopete/kitbag'
$version = if ($env:KITBAG_VERSION) { $env:KITBAG_VERSION } else { 'latest' }
$dir = if ($env:KITBAG_INSTALL_DIR) { $env:KITBAG_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'kitbag\bin' }
$arch = if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq 'Arm64') { 'aarch64' } else { 'x86_64' }
$archive = "kitbag-$arch-pc-windows-msvc.zip"
$base = if ($version -eq 'latest') { "https://github.com/$repo/releases/latest/download" } else { "https://github.com/$repo/releases/download/$version" }
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
  Invoke-WebRequest "$base/$archive" -OutFile (Join-Path $tmp $archive)
  Invoke-WebRequest "$base/$archive.sha256" -OutFile (Join-Path $tmp "$archive.sha256")
  $expected = (Get-Content (Join-Path $tmp "$archive.sha256")).Split(' ')[0].ToLower()
  $actual = (Get-FileHash (Join-Path $tmp $archive) -Algorithm SHA256).Hash.ToLower()
  if ($expected -ne $actual) { throw "checksum mismatch for $archive" }
  Expand-Archive (Join-Path $tmp $archive) -DestinationPath $tmp -Force
  New-Item -ItemType Directory -Path $dir -Force | Out-Null
  $exe = Get-ChildItem $tmp -Recurse -Filter 'kitbag.exe' | Select-Object -First 1
  Copy-Item $exe.FullName (Join-Path $dir 'kitbag.exe') -Force
  Write-Host "installed $(& (Join-Path $dir 'kitbag.exe') --version) to $dir"
  if (-not ($env:PATH -split ';' -contains $dir)) { Write-Host "note: add $dir to your PATH" }
} finally {
  Remove-Item $tmp -Recurse -Force
}
