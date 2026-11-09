# Install thirdeye on Windows from a release, then refresh its agent
# integrations.
#
#   irm https://github.com/lividgreen/thirdeye/releases/latest/download/install.ps1 | iex
#
# What install.sh is elsewhere. It verifies the archive's checksum, puts the
# binary in %LOCALAPPDATA%\Programs\thirdeye, adds that directory to the
# *user* PATH through the registry (not `setx`, which truncates), and hands
# over to the new binary's `thirdeye refresh`.
#
# Knobs, as environment variables (`irm | iex` takes no arguments):
#   THIRDEYE_VERSION       a release, e.g. 0.2.0 (default: the latest)
#   THIRDEYE_INSTALL_DIR   where the binary goes
#   THIRDEYE_BASE_URL      where the archives are (default: the GitHub release)
#   THIRDEYE_ARCHIVE       install this .zip instead of downloading one;
#                          a .sha256 beside it is checked if present

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Say($text) { Write-Host "thirdeye: $text" }

$releases = 'https://github.com/lividgreen/thirdeye/releases'
$version = "$env:THIRDEYE_VERSION".TrimStart('v')
$dir = if ($env:THIRDEYE_INSTALL_DIR) { $env:THIRDEYE_INSTALL_DIR } else {
    Join-Path $env:LOCALAPPDATA 'Programs\thirdeye'
}
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64' -or $env:PROCESSOR_ARCHITEW6432 -eq 'ARM64') {
    'aarch64'
} else {
    'x86_64'
}
$name = "thirdeye-$arch-pc-windows-msvc.zip"
$base = if ($env:THIRDEYE_BASE_URL) { $env:THIRDEYE_BASE_URL.TrimEnd('/') }
        elseif ($version) { "$releases/download/v$version" }
        else { "$releases/latest/download" }

$work = Join-Path ([System.IO.Path]::GetTempPath()) ("thirdeye-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $work | Out-Null
try {
    $zip = Join-Path $work $name
    $sum = "$zip.sha256"
    if ($env:THIRDEYE_ARCHIVE) {
        Copy-Item $env:THIRDEYE_ARCHIVE $zip
        if (Test-Path "$($env:THIRDEYE_ARCHIVE).sha256") {
            Copy-Item "$($env:THIRDEYE_ARCHIVE).sha256" $sum
        } else {
            Say "warning: no .sha256 beside the archive; not verified"
        }
    } else {
        Say "downloading $base/$name"
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$name" -OutFile $zip
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$name.sha256" -OutFile $sum
    }
    if (Test-Path $sum) {
        $want = ((Get-Content $sum -Raw).Trim() -split '\s+')[0].ToLower()
        $have = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
        if ($want -ne $have) {
            throw "checksum mismatch for $name (expected $want, got $have); nothing installed"
        }
    }
    Expand-Archive -Path $zip -DestinationPath (Join-Path $work 'x') -Force
    $new = Join-Path $work 'x\thirdeye.exe'
    if (-not (Test-Path $new)) { throw "$name holds no thirdeye.exe" }

    New-Item -ItemType Directory -Path $dir -Force | Out-Null
    $exe = Join-Path $dir 'thirdeye.exe'
    $first = -not (Test-Path $exe)
    # What an earlier update moved aside, now that nothing runs it.
    Get-ChildItem $dir -Filter 'thirdeye.exe.old*' -ErrorAction SilentlyContinue |
        Remove-Item -Force -ErrorAction SilentlyContinue
    # A daemon or a viewer may be running the old binary, and Windows will
    # not overwrite a running executable; it will rename one.
    $aside = $null
    if (-not $first) {
        $aside = Join-Path $dir ("thirdeye.exe.old-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
        Rename-Item $exe (Split-Path $aside -Leaf)
    }
    try { Copy-Item $new $exe } catch {
        # Never leave no binary at all: every hook names this path.
        if ($aside) { Move-Item $aside $exe -Force -ErrorAction SilentlyContinue }
        throw
    }
    # Gone at once unless something still runs it — `thirdeye self update`
    # always does, being the caller; then the sweep above takes it next time.
    if ($aside) { Remove-Item $aside -Force -ErrorAction SilentlyContinue }
    Say "installed $(& $exe --version) at $exe"

    # The new binary refreshes: it is the one the integrations must load.
    & $exe refresh --installed-by script | Out-Null
    if ($LASTEXITCODE -ne 0) { Say "warning: ``thirdeye refresh`` failed; run it to see why" }

    # The user PATH, read and written unexpanded so entries such as
    # %USERPROFILE%\bin stay as they were.
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
    $path = [string]$key.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
    # @(): with one entry the pipeline yields a string, and `+` below would
    # glue the new directory onto it.
    $entries = @($path -split ';' | Where-Object { $_ })
    if ($entries -notcontains $dir) {
        $key.SetValue('Path', (($entries + $dir) -join ';'), 'ExpandString')
        # Tell running programs the environment changed; a new terminal sees it.
        [Environment]::SetEnvironmentVariable('THIRDEYE_INSTALL_BROADCAST', '1', 'User')
        [Environment]::SetEnvironmentVariable('THIRDEYE_INSTALL_BROADCAST', $null, 'User')
        Say "added $dir to your PATH; open a new terminal to use ``thirdeye``"
    }
    $key.Close()

    if ($first) {
        # `thirdeye install` asks about each agent it finds; only with a
        # console to ask on, and not from the home directory, no project.
        $here = (Get-Location).ProviderPath
        # The repository's root, as the agents take it, from anywhere inside.
        try {
            $top = & git -C $here rev-parse --show-toplevel 2>$null
            if ($LASTEXITCODE -eq 0 -and $top) { $here = [IO.Path]::GetFullPath($top) }
        } catch {}
        if ([Environment]::UserInteractive -and $Host.Name -eq 'ConsoleHost' -and
            $here -ne $env:USERPROFILE) {
            & $exe install $here
            if ($LASTEXITCODE -ne 0) { Say 'later: `thirdeye install <project>`' }
        } else {
            Say 'next: `thirdeye install <project>` finds your agents and asks which to wire'
        }
    }
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
