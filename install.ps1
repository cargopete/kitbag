# Install kitbag on Windows from GitHub Releases.
#   irm https://raw.githubusercontent.com/cargopete/kitbag/main/install.ps1 | iex
# Environment: KITBAG_VERSION (default latest), KITBAG_INSTALL_DIR (default %LOCALAPPDATA%\kitbag\bin)
$ErrorActionPreference = 'Stop'
$repo = 'cargopete/kitbag'
$version = if ($env:KITBAG_VERSION) { $env:KITBAG_VERSION } else { 'latest' }
$dir = if ($env:KITBAG_INSTALL_DIR) { $env:KITBAG_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'kitbag\bin' }
$arch = if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq 'Arm64') { 'aarch64' } else { 'x86_64' }
$archive = "kitbag-$arch-pc-windows-msvc.zip"
$checksum = "kitbag-$arch-pc-windows-msvc.sha256"
$base = if ($version -eq 'latest') { "https://github.com/$repo/releases/latest/download" } else { "https://github.com/$repo/releases/download/$version" }
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
  Invoke-WebRequest "$base/$archive" -OutFile (Join-Path $tmp $archive)
  Invoke-WebRequest "$base/$checksum" -OutFile (Join-Path $tmp $checksum)
  # Fail closed: both sides must be a real SHA-256 (64 hex digits), so an empty or
  # garbled checksum file, or a missing or failing hasher, can't pass.
  if (-not (Get-Command Get-FileHash -ErrorAction SilentlyContinue)) { throw 'Get-FileHash is required to verify the download (PowerShell 4 or later)' }
  $expected = "$(Get-Content (Join-Path $tmp $checksum) -Raw)".Trim().Split(' ')[0].ToLower()
  if ($expected -notmatch '^[0-9a-f]{64}$') { throw "$checksum does not hold a SHA-256 checksum" }
  $actual = "$((Get-FileHash (Join-Path $tmp $archive) -Algorithm SHA256).Hash)".ToLower()
  if ($actual -notmatch '^[0-9a-f]{64}$') { throw "could not compute the SHA-256 of $archive" }
  if ($expected -ne $actual) { throw "checksum mismatch for $archive (expected $expected, got $actual)" }
  Expand-Archive (Join-Path $tmp $archive) -DestinationPath $tmp -Force
  New-Item -ItemType Directory -Path $dir -Force | Out-Null
  $exe = Get-ChildItem $tmp -Recurse -Filter 'kitbag.exe' | Select-Object -First 1
  if (-not $exe) { throw "archive did not contain kitbag.exe" }
  # Replace atomically: copy to a new temp file next to the target, then swap it
  # in with one rename, so an interrupted install never leaves a half-written exe.
  $dest = Join-Path $dir 'kitbag.exe'
  $new = Join-Path $dir ".kitbag.exe.$([System.Guid]::NewGuid()).tmp"
  try {
    Copy-Item -LiteralPath $exe.FullName -Destination $new
    if (Test-Path -LiteralPath $dest) {
      [System.IO.File]::Replace($new, $dest, [NullString]::Value)
    } else {
      [System.IO.File]::Move($new, $dest)
    }
  } finally {
    if (Test-Path -LiteralPath $new) { Remove-Item -LiteralPath $new -Force }
  }
  Write-Host "installed $(& $dest --version) to $dir"
  if (-not ($env:PATH -split ';' -contains $dir)) { Write-Host "note: add $dir to your PATH" }
} finally {
  Remove-Item $tmp -Recurse -Force
}
