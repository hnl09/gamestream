# Monta o app Android (GameStream.apk) sem Gradle: núcleo em Rust (NDK) + Java (javac/d8) +
# recursos (aapt2), alinhado e assinado. Uso:  .\android\build.ps1 [-Install]
# Requer: Android SDK em %LOCALAPPDATA%\Android\Sdk (platforms, build-tools, ndk), JDK 17, rustup com
# o alvo aarch64-linux-android.
param([switch]$Install, [string]$VersionName = "0.1", [int]$VersionCode = 1)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$repo = Split-Path -Parent $root
$sdk = if ($env:ANDROID_HOME) { $env:ANDROID_HOME } else { "$env:LOCALAPPDATA\Android\Sdk" }
$ndk = (Get-ChildItem "$sdk\ndk" | Sort-Object Name | Select-Object -Last 1).FullName
$bt = (Get-ChildItem "$sdk\build-tools" | Sort-Object Name | Select-Object -Last 1).FullName
$jar = (Get-ChildItem "$sdk\platforms" | Sort-Object Name | Select-Object -Last 1).FullName + "\android.jar"
if (-not $env:JAVA_HOME) { $env:JAVA_HOME = (Split-Path -Parent (Split-Path -Parent (Get-Command javac).Source)) }
$jbin = "$env:JAVA_HOME\bin"

# 1) Núcleo em Rust para ARM64 (celulares Android atuais).
$tc = "$ndk\toolchains\llvm\prebuilt\windows-x86_64\bin"
$env:CC_aarch64_linux_android = "$tc\aarch64-linux-android26-clang.cmd"
$env:AR_aarch64_linux_android = "$tc\llvm-ar.exe"
$env:CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER = "$tc\aarch64-linux-android26-clang.cmd"
$env:CARGO_TARGET_DIR = "$env:USERPROFILE\.gamestream-android-target"
cargo build --release --target aarch64-linux-android --manifest-path "$root\core\Cargo.toml"
if ($LASTEXITCODE -ne 0) { throw "falha ao compilar o núcleo em Rust" }
$so = "$env:CARGO_TARGET_DIR\aarch64-linux-android\release\libgamestream.so"

# 2) Java -> classes -> dex.
$out = "$root\build"
Remove-Item -Recurse -Force $out -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force "$out\classes", "$out\dex", "$out\res\mipmap" | Out-Null
$java = Get-ChildItem "$root\app\src" -Recurse -Filter *.java | ForEach-Object { $_.FullName }
& "$jbin\javac.exe" --release 11 -encoding UTF-8 -nowarn -classpath $jar -d "$out\classes" @java
if ($LASTEXITCODE -ne 0) { throw "falha ao compilar o Java" }
$classes = Get-ChildItem "$out\classes" -Recurse -Filter *.class | ForEach-Object { $_.FullName }
& "$bt\d8.bat" --release --min-api 26 --lib $jar --output "$out\dex" @classes
if ($LASTEXITCODE -ne 0) { throw "falha no d8" }

# 3) Recursos (ícone) e manifesto.
Copy-Item "$repo\assets\icon.png" "$out\res\mipmap\ic_launcher.png"
& "$bt\aapt2.exe" compile --dir "$out\res" -o "$out\res.zip"
& "$bt\aapt2.exe" link -o "$out\base.apk" -I $jar --manifest "$root\app\AndroidManifest.xml" -R "$out\res.zip" `
  --min-sdk-version 26 --target-sdk-version 35 --version-code $VersionCode --version-name $VersionName --auto-add-overlay
if ($LASTEXITCODE -ne 0) { throw "falha no aapt2 link" }

# 4) Junta dex e a biblioteca nativa no APK.
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
$z = [IO.Compression.ZipFile]::Open("$out\base.apk", "Update")
[IO.Compression.ZipFileExtensions]::CreateEntryFromFile($z, "$out\dex\classes.dex", "classes.dex") | Out-Null
[IO.Compression.ZipFileExtensions]::CreateEntryFromFile($z, $so, "lib/arm64-v8a/libgamestream.so") | Out-Null
$z.Dispose()

# 5) Alinha e assina (chave de desenvolvimento guardada fora do repositório).
& "$bt\zipalign.exe" -p -f 4 "$out\base.apk" "$out\aligned.apk"
$ks = "$env:LOCALAPPDATA\GameStream\android-debug.keystore"
if (-not (Test-Path $ks)) {
  New-Item -ItemType Directory -Force (Split-Path $ks) | Out-Null
  & "$jbin\keytool.exe" -genkeypair -keystore $ks -storepass android -keypass android -alias gamestream -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=GameStream" | Out-Null
}
& "$bt\apksigner.bat" sign --ks $ks --ks-pass pass:android --key-pass pass:android --out "$out\GameStream.apk" "$out\aligned.apk"
if ($LASTEXITCODE -ne 0) { throw "falha ao assinar" }
"APK: $out\GameStream.apk ($([math]::Round((Get-Item "$out\GameStream.apk").Length/1MB,1)) MB)"

# Cópia publicada nas releases do GitHub (o workflow anexa android/GameStream.apk).
Copy-Item "$out\GameStream.apk" "$root\GameStream.apk" -Force

if ($Install) {
  adb install -r "$out\GameStream.apk"
}
