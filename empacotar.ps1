# Compila e monta os mesmos arquivos que a release do GitHub publica (pasta out\):
#   GameStream.zip            app + player (sem FFmpeg) - pacote distribuído aos usuários
#   gamestream.exe (+.sha256) usado pela atualização automática
#   ffmpeg.exe.gz (+.sha256)  baixado pelo app na primeira transmissão
# Também deixa em dist\ uma cópia pronta para testar (com o ffmpeg.exe local, se houver).
# Uso:  powershell -ExecutionPolicy Bypass -File .\empacotar.ps1
$ErrorActionPreference = "Stop"
$target = "$env:USERPROFILE\.gamestream-target"   # caminho curto: evita erro de "path too long" no Windows
$env:CARGO_TARGET_DIR = $target
& "$env:USERPROFILE\.cargo\bin\cargo.exe" build --release
if ($LASTEXITCODE -ne 0) { throw "falha na compilação" }

$dist = Join-Path $PSScriptRoot "dist"
$out = Join-Path $PSScriptRoot "out"
Remove-Item $dist, $out -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $dist, $out | Out-Null
Copy-Item "$target\release\gamestream.exe" $dist

$mpv = (Get-ChildItem "$env:LOCALAPPDATA\Microsoft\WinGet\Packages" -Recurse -Filter mpv.exe -ErrorAction SilentlyContinue | Select-Object -First 1)
if ($mpv) { Copy-Item "$($mpv.DirectoryName)\mpv.exe", "$($mpv.DirectoryName)\vulkan-1.dll" $dist } else { Write-Warning "mpv.exe não encontrado" }
Copy-Item (Join-Path $PSScriptRoot "LEIA-ME.txt"), (Join-Path $PSScriptRoot "THIRD-PARTY.txt") $dist
Compress-Archive -Path "$dist\*" -DestinationPath "$out\GameStream.zip" -Force

Copy-Item "$target\release\gamestream.exe" $out
(Get-FileHash "$out\gamestream.exe" -Algorithm SHA256).Hash.ToLower() | Out-File -NoNewline -Encoding ascii "$out\gamestream.exe.sha256"

$ffmpeg = (Get-Command ffmpeg -ErrorAction SilentlyContinue).Source
if ($ffmpeg) {
    $in = [IO.File]::OpenRead($ffmpeg); $o = [IO.File]::Create("$out\ffmpeg.exe.gz")
    $gz = New-Object IO.Compression.GZipStream($o, [IO.Compression.CompressionLevel]::Optimal)
    $in.CopyTo($gz); $gz.Dispose(); $in.Dispose(); $o.Dispose()
    (Get-FileHash "$out\ffmpeg.exe.gz" -Algorithm SHA256).Hash.ToLower() | Out-File -NoNewline -Encoding ascii "$out\ffmpeg.exe.gz.sha256"
    Copy-Item $ffmpeg $dist   # só na cópia local de teste
} else { Write-Warning "ffmpeg.exe não encontrado" }

Get-ChildItem $out | ForEach-Object { "{0,-26} {1,8:N1} MB" -f $_.Name, ($_.Length / 1MB) }
