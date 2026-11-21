# Installs dune and dunx on Windows from the j8 GitHub releases.
#   powershell -c "irm https://j8.app238.com/install.ps1 | iex"
#   & ([scriptblock]::Create((irm https://j8.app238.com/install.ps1))) -Version v0.5.0
# $env:DUNE_INSTALL sets the install root (default ~\.dune, binaries in its bin\);
# $env:DUNE_RELEASES sets where releases are downloaded from (default the j8 GitHub releases).
param(
  [string]$Version = "latest",
  [switch]$NoPathUpdate
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$Releases = if ($env:DUNE_RELEASES) { $env:DUNE_RELEASES } else { "https://github.com/0x00-Community/j8/releases" }
$Root = if ($env:DUNE_INSTALL) { $env:DUNE_INSTALL } else { Join-Path $HOME ".dune" }
$Bin = Join-Path $Root "bin"

$Arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
$Cpu = switch ($Arch) {
  "AMD64" { "x64" }
  "ARM64" { "arm64" }
  default { throw "dune: unsupported CPU: $Arch" }
}

$Target = "dune-win32-$Cpu"
$Archive = "$Target.tar.gz"
if ($Version -eq "latest") {
  $Url = "$Releases/latest/download"
} else {
  if (-not $Version.StartsWith("v")) { $Version = "v$Version" }
  $Url = "$Releases/download/$Version"
}

if (-not (Get-Command tar.exe -ErrorAction SilentlyContinue)) {
  throw "dune: tar.exe is required (built into Windows 10 1803 and later)"
}

$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("dune-" + [System.Guid]::NewGuid())
New-Item -ItemType Directory -Path $Tmp | Out-Null
try {
  Write-Host "Downloading $Url/$Archive" -ForegroundColor DarkGray
  try {
    Invoke-WebRequest -UseBasicParsing -Uri "$Url/$Archive" -OutFile (Join-Path $Tmp $Archive)
  } catch {
    throw "dune: failed to download $Archive ($Version); is there a release for win32-$Cpu?"
  }

  $Sums = Join-Path $Tmp "SHA256SUMS"
  try { Invoke-WebRequest -UseBasicParsing -Uri "$Url/SHA256SUMS" -OutFile $Sums } catch { $Sums = $null }
  if ($Sums) {
    $Line = Get-Content $Sums | Where-Object { ($_ -split '\s+')[1] -and ($_ -split '\s+')[1].TrimStart('*') -eq $Archive } | Select-Object -First 1
    if ($Line) {
      $Expected = ($Line -split '\s+')[0].ToLower()
      $Actual = (Get-FileHash -Algorithm SHA256 (Join-Path $Tmp $Archive)).Hash.ToLower()
      if ($Expected -ne $Actual) { throw "dune: checksum mismatch for $Archive" }
    }
  }

  tar.exe -xzf (Join-Path $Tmp $Archive) -C $Tmp
  if ($LASTEXITCODE -ne 0) { throw "dune: failed to extract $Archive" }

  New-Item -ItemType Directory -Force -Path $Bin | Out-Null
  $Dune = Join-Path $Bin "dune.exe"
  $Dunx = Join-Path $Bin "dunx.exe"
  foreach ($Old in $Dune, $Dunx) {
    if (Test-Path $Old) { Move-Item -Force $Old "$Old.old"; Remove-Item -Force "$Old.old" -ErrorAction SilentlyContinue }
  }
  Move-Item -Force (Join-Path $Tmp "$Target\dune.exe") $Dune
  try { New-Item -ItemType HardLink -Path $Dunx -Target $Dune | Out-Null } catch { Copy-Item $Dune $Dunx }
} finally {
  Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}

$Installed = & $Dune --version 2>$null
Write-Host "$(if ($Installed) { $Installed } else { 'dune' }) was installed to $Dune (and dunx)" -ForegroundColor Green

if (-not $NoPathUpdate) {
  $Path = [Environment]::GetEnvironmentVariable("Path", "User")
  $Parts = if ($Path) { $Path -split ';' | Where-Object { $_ } } else { @() }
  if ($Parts -notcontains $Bin) {
    [Environment]::SetEnvironmentVariable("Path", (@($Bin) + $Parts) -join ';', "User")
    [Environment]::SetEnvironmentVariable("DUNE_INSTALL", $Root, "User")
    $env:Path = "$Bin;$env:Path"
    Write-Host "Added $Bin to your user PATH; open a new terminal to use it" -ForegroundColor DarkGray
  }
}

Write-Host ""
Write-Host "Run 'dune --help' to get started"
