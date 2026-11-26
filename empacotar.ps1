# Compila e monta a pasta "dist" para mandar aos amigos (zipada em GameStream.zip).
# Uso:  powershell -ExecutionPolicy Bypass -File .\empacotar.ps1
$ErrorActionPreference = "Stop"
$target = "$env:USERPROFILE\.gamestream-target"   # caminho curto: evita erro de "path too long" no Windows
$env:CARGO_TARGET_DIR = $target
& "$env:USERPROFILE\.cargo\bin\cargo.exe" build --release
if ($LASTEXITCODE -ne 0) { throw "falha na compilação" }

$dist = Join-Path $PSScriptRoot "dist"
Remove-Item $dist -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $dist | Out-Null
Copy-Item "$target\release\gamestream.exe" $dist

$mpv = (Get-ChildItem "$env:LOCALAPPDATA\Microsoft\WinGet\Packages" -Recurse -Filter mpv.exe -ErrorAction SilentlyContinue | Select-Object -First 1)
if ($mpv) { Copy-Item "$($mpv.DirectoryName)\mpv.exe", "$($mpv.DirectoryName)\vulkan-1.dll" $dist } else { Write-Warning "mpv.exe não encontrado" }

$ffmpeg = (Get-Command ffmpeg -ErrorAction SilentlyContinue).Source
if ($ffmpeg) { Copy-Item $ffmpeg $dist } else { Write-Warning "ffmpeg.exe não encontrado (só é necessário para quem for transmitir)" }

Copy-Item (Join-Path $PSScriptRoot "LEIA-ME.txt"), (Join-Path $PSScriptRoot "THIRD-PARTY.txt") $dist
Compress-Archive -Path "$dist\*" -DestinationPath (Join-Path $PSScriptRoot "GameStream.zip") -Force
Write-Host "Pronto: dist\ e GameStream.zip"
