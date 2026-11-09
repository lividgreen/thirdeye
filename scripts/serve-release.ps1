# Serve a directory of release assets on 127.0.0.1, for testing install.ps1
# and `thirdeye self update` before a release can be fetched from GitHub.
#
#   gh release download -R lividgreen/thirdeye -D $env:TEMP\te-release
#   powershell -NoProfile -File scripts\serve-release.ps1 $env:TEMP\te-release
#   $env:THIRDEYE_BASE_URL = 'http://127.0.0.1:8765'
#
# Not `python -m http.server`: a stock Windows has no Python, only the
# Store's alias named `python`. Not HttpListener either, which needs an
# administrator's URL reservation to listen on anything but `localhost`.
# A TcpListener needs neither. GET only, files only, nothing above -Root.
param([Parameter(Mandatory)][string]$Root, [int]$Port = 8765)
$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path $Root).ProviderPath.TrimEnd('\') + '\'
$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $Port)
$listener.Start()
Write-Host "serving $Root on http://127.0.0.1:$Port (Ctrl+C stops)"
try {
    while ($true) {
        $client = $listener.AcceptTcpClient()
        try {
            $stream = $client.GetStream()
            $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::ASCII, $false, 4096, $true)
            $request = "$($reader.ReadLine())" -split ' '
            while (($line = $reader.ReadLine()) -ne $null -and $line -ne '') {}
            $rel = [Uri]::UnescapeDataString(($request[1] -split '\?')[0]).TrimStart('/')
            $full = [System.IO.Path]::GetFullPath((Join-Path $Root $rel))
            if ($request[0] -eq 'GET' -and $rel -and $full.StartsWith($Root, 'OrdinalIgnoreCase') -and
                (Test-Path -LiteralPath $full -PathType Leaf)) {
                $status = '200 OK'
                $body = [System.IO.File]::ReadAllBytes($full)
            } else {
                $status = '404 Not Found'
                $body = [System.Text.Encoding]::ASCII.GetBytes("not found`n")
            }
            $head = [System.Text.Encoding]::ASCII.GetBytes(
                "HTTP/1.1 $status`r`nContent-Length: $($body.Length)`r`n" +
                "Content-Type: application/octet-stream`r`nConnection: close`r`n`r`n")
            $stream.Write($head, 0, $head.Length)
            $stream.Write($body, 0, $body.Length)
            Write-Host "$status /$rel"
        } catch {
            Write-Host "error: $_"
        } finally {
            $client.Close()
        }
    }
} finally {
    $listener.Stop()
}
