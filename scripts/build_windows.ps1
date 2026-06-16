$ErrorActionPreference = "Continue"

& pnpm tauri build --target x86_64-pc-windows-msvc --bundles nsis *> tauri-build.log
$buildExitCode = $LASTEXITCODE

Get-Content tauri-build.log -Tail 180

if ($buildExitCode -ne 0) {
    throw "Tauri build failed with exit code $buildExitCode"
}
