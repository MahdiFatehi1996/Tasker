# tasker installer for Windows.
#
#   irm https://github.com/MahdiFatehi1996/Tasker/releases/latest/download/tasker-install.ps1 | iex
#
# Resolves the release, downloads the archive built for this machine, verifies
# it against SHA256SUMS.txt, unpacks tasker.exe into %USERPROFILE%\.local\bin,
# and puts that directory on your user PATH if it is not already there.
#
# Parameters work when the script is run as a file:
#
#   .\tasker-install.ps1 -Version v0.3.2 -Dir C:\tools
#
# Piped into Invoke-Expression there is no way to pass them, so TASKER_VERSION
# and TASKER_INSTALL_DIR override everything:
#
#   $env:TASKER_VERSION = 'v0.3.2'; irm .../tasker-install.ps1 | iex
#
param(
  [string]$Version = '',
  [string]$Dir = '',
  [switch]$Help
)

# Invoke-Expression runs in the caller's scope, so without this wrapper the
# script would leave $Repo, $destDir, $scratch and friends behind in an
# interactive session — and clobber any caller variable of the same name.
# Everything the installer does stays inside this block.
& {
  $Repo = 'MahdiFatehi1996/Tasker'
  $DefaultDir = Join-Path $HOME '.local\bin'

  function Show-Usage {
    Write-Output @"
Usage: tasker-install.ps1 [-Version <tag>] [-Dir <path>]

Options:
  -Version <tag>  Install a specific release, e.g. -Version v0.3.2.
                  Defaults to the latest release. `$env:TASKER_VERSION does the same.
  -Dir <path>     Install into <path> instead of $DefaultDir.
                  `$env:TASKER_INSTALL_DIR does the same.
  -Help           Show this help.
"@
  }

  function Resolve-LatestTag {
    try {
      $resp = Invoke-WebRequest -Uri "https://github.com/$Repo/releases/latest" -UseBasicParsing -ErrorAction Stop
      $uri = $null
      if ($resp.BaseResponse.ResponseUri) { $uri = $resp.BaseResponse.ResponseUri.AbsoluteUri }
      if (-not $uri -and $resp.BaseResponse.RequestMessage) {
        $uri = $resp.BaseResponse.RequestMessage.RequestUri.AbsoluteUri
      }
      if ($uri -and $uri -match '/releases/tag/(?<tag>v[^/]+)$') { return $Matches['tag'] }
    } catch { }

    try {
      $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -ErrorAction Stop
      if ($rel.tag_name) { return [string]$rel.tag_name }
    } catch { }

    return $null
  }

  function Invoke-Download { # url dest
    param([string]$Url, [string]$Dest)
    try {
      Invoke-WebRequest -Uri $Url -OutFile $Dest -UseBasicParsing -ErrorAction Stop
    } catch {
      throw "download failed: $Url`n$($_.Exception.Message)"
    }
  }

  if ($Help) {
    Show-Usage
    return
  }

  $installedVersion = $Version
  if (-not $installedVersion) { $installedVersion = $env:TASKER_VERSION }
  if (-not $installedVersion) {
    Write-Host 'resolving the latest release of tasker'
    $installedVersion = Resolve-LatestTag
    if (-not $installedVersion) {
      throw 'could not determine the latest release tag (use -Version, or set TASKER_VERSION)'
    }
  }
  if ($installedVersion -notlike 'v*') { $installedVersion = "v$installedVersion" }

  $destDir = $Dir
  if (-not $destDir) { $destDir = $env:TASKER_INSTALL_DIR }
  if (-not $destDir) { $destDir = $DefaultDir }

  $machineArch = $env:PROCESSOR_ARCHITECTURE
  # In a 32-bit PowerShell on x64 Windows PROCESSOR_ARCHITECTURE reads x86;
  # PROCESSOR_ARCHITEW6432 then carries the real architecture.
  if ($env:PROCESSOR_ARCHITEW6432) { $machineArch = $env:PROCESSOR_ARCHITEW6432 }
  switch -Regex ($machineArch) {
    'AMD64' { $target = 'x86_64-pc-windows-msvc'; break }
    'ARM64' {
      # There is no ARM64 build yet; x64 binaries run under emulation.
      $target = 'x86_64-pc-windows-msvc'
      Write-Warning "no ARM64 build of tasker yet; installing the x64 build for emulation"
      break
    }
    default { throw "unsupported CPU architecture: $machineArch" }
  }

  $archive = "tasker-$target.zip"
  $binName = 'tasker.exe'
  $base = "https://github.com/$Repo/releases/download/$installedVersion"

  Write-Host "installing tasker $installedVersion ($target)"

  $scratch = Join-Path ([IO.Path]::GetTempPath()) ('tasker-install-' + [guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Path $scratch -Force | Out-Null

  # Stop on any cmdlet failure. The preference is local to the block above, so
  # the caller's session is unaffected either way.
  $ErrorActionPreference = 'Stop'

  try {
    Invoke-Download "$base/$archive" (Join-Path $scratch $archive)
    Invoke-Download "$base/SHA256SUMS.txt" (Join-Path $scratch 'SHA256SUMS.txt')

    $expected = $null
    foreach ($line in Get-Content (Join-Path $scratch 'SHA256SUMS.txt')) {
      if ($line -match "^(?<sum>[0-9a-fA-F]{64})\s+(?<name>\S+)$" -and $Matches['name'] -eq $archive) {
        $expected = $Matches['sum']
        break
      }
    }
    if (-not $expected) { throw "$archive is missing from SHA256SUMS.txt" }

    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $scratch $archive)).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
      throw "checksum mismatch for ${archive}: expected $expected, got $actual"
    }
    Write-Host 'checksum verified'

    $extractDir = Join-Path $scratch 'extracted'
    Expand-Archive -LiteralPath (Join-Path $scratch $archive) -DestinationPath $extractDir -Force
    $sourceBin = Join-Path $extractDir $binName
    if (-not (Test-Path -LiteralPath $sourceBin)) { throw "the archive did not contain $binName" }

    New-Item -ItemType Directory -Path $destDir -Force | Out-Null
    Copy-Item -LiteralPath $sourceBin -Destination (Join-Path $destDir $binName) -Force

    # Put the install directory on the user PATH when it is not there already.
    # This only affects new shells; the current one keeps its PATH as it is.
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $onPath = (";${userPath};" -like "*;$destDir;*") -or (";${env:PATH};" -like "*;$destDir;*")
    if (-not $onPath) {
      $entries = @($userPath -split ';' | Where-Object { $_ -and $_.TrimEnd('\') -ne $destDir.TrimEnd('\') })
      $entries += $destDir
      [Environment]::SetEnvironmentVariable('Path', ($entries -join ';'), 'User')
      Write-Warning "$destDir was added to your user PATH; open a new terminal for it to take effect"
    }

    # Smoke test against a throwaway database so the real one is never touched.
    $probe = Join-Path $scratch 'probe.db'
    $probeOut = & (Join-Path $destDir $binName) --db $probe capabilities --json 2>&1
    $probeOk = ($LASTEXITCODE -eq 0)
    # The probe's exit code must not outlive it, or callers reading
    # $LASTEXITCODE would see a failed install that actually succeeded.
    $global:LASTEXITCODE = 0
    if ($probeOk) {
      Write-Host "verified $destDir\$binName runs"
    } else {
      Write-Warning "installed $destDir\$binName but could not run it here"
      Write-Warning "try running 'tasker' manually, or open an issue at https://github.com/$Repo/issues"
    }
  } finally {
    Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
  }

  Write-Host "tasker installed to $destDir"
}
