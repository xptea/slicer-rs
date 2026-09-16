# Assemble a Windows Slicer directory and ZIP archive with private FFmpeg
# tools. The script never copies ffmpeg.exe or ffprobe.exe from PATH.
[CmdletBinding()]
param(
    [string]$Binary = '',
    [string]$FfmpegBundle = '',
    [string]$RustNotices = '',
    [string]$Dist = '',
    [string]$Name = '',
    [int]$Jobs = 0
)

$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$LockFile = Join-Path $Root 'packaging\ffmpeg.lock'

function Get-LockedValue([string]$Key) {
    $pattern = '^' + [regex]::Escape($Key) + '=(.*)$'
    foreach ($line in Get-Content -LiteralPath $LockFile) {
        if ($line -match $pattern) {
            return $Matches[1]
        }
    }
    throw "Missing $Key in $LockFile"
}

$FfmpegVersion = Get-LockedValue 'FFMPEG_VERSION'
$SourceArchiveName = Get-LockedValue 'FFMPEG_SOURCE_ARCHIVE'
$SourceSha256 = (Get-LockedValue 'FFMPEG_SOURCE_SHA256').ToLowerInvariant()

if ([string]::IsNullOrWhiteSpace($Binary)) {
    $release = Join-Path $Root 'target\release\slicer.exe'
    $debug = Join-Path $Root 'target\debug\slicer.exe'
    if (Test-Path -LiteralPath $release -PathType Leaf) {
        $Binary = $release
    } elseif (Test-Path -LiteralPath $debug -PathType Leaf) {
        Write-Warning 'Release binary is absent; packaging target\debug\slicer.exe'
        $Binary = $debug
    } else {
        throw 'No Slicer executable. Pass -Binary PATH\slicer.exe.'
    }
}
$Binary = (Resolve-Path -LiteralPath $Binary).Path

if ([string]::IsNullOrWhiteSpace($FfmpegBundle)) {
    $FfmpegBundle = Join-Path $Root 'build\ffmpeg\windows-x86_64'
}
if ([string]::IsNullOrWhiteSpace($RustNotices)) {
    $RustNotices = Join-Path $Root 'build\rust-notices'
}
if ([string]::IsNullOrWhiteSpace($Dist)) {
    $Dist = Join-Path $Root 'dist'
}
if ([string]::IsNullOrWhiteSpace($Name)) {
    $Name = 'slicer-windows-x86_64'
}

$ffmpeg = Join-Path $FfmpegBundle 'bin\ffmpeg.exe'
$ffprobe = Join-Path $FfmpegBundle 'bin\ffprobe.exe'
if (!(Test-Path -LiteralPath $ffmpeg -PathType Leaf) -or
    !(Test-Path -LiteralPath $ffprobe -PathType Leaf)) {
    $bash = Get-Command bash -ErrorAction SilentlyContinue
    if ($null -eq $bash) {
        throw "FFmpeg bundle is missing. Build it with scripts/build-ffmpeg.sh --target windows-x86_64 from a MinGW shell, then pass -FfmpegBundle."
    }
    $buildScript = Join-Path $Root 'scripts\build-ffmpeg.sh'
    $buildArgs = @($buildScript, '--target', 'windows-x86_64', '--output', $FfmpegBundle)
    if ($Jobs -gt 0) { $buildArgs += @('--jobs', $Jobs.ToString()) }
    & $bash.Source @buildArgs
    if ($LASTEXITCODE -ne 0) {
        throw "FFmpeg build failed with exit code $LASTEXITCODE"
    }
}

foreach ($program in @($ffmpeg, $ffprobe)) {
    if (!(Test-Path -LiteralPath $program -PathType Leaf)) {
        throw "FFmpeg bundle lacks $program"
    }
}
$sourceArchive = Join-Path $FfmpegBundle (Join-Path 'source' $SourceArchiveName)
if (!(Test-Path -LiteralPath $sourceArchive -PathType Leaf)) {
    throw "Pinned FFmpeg source archive is missing: $sourceArchive"
}
$actualHash = (Get-FileHash -LiteralPath $sourceArchive -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actualHash -ne $SourceSha256) {
    throw "FFmpeg source SHA-256 mismatch. Expected $SourceSha256, got $actualHash"
}
foreach ($material in @(
        (Join-Path $FfmpegBundle 'COPYING.LGPLv2.1'),
        (Join-Path $FfmpegBundle 'FFMPEG-NOTICE.txt'),
        (Join-Path $FfmpegBundle 'source\PROVENANCE.txt'))) {
    if (!(Test-Path -LiteralPath $material -PathType Leaf)) {
        throw "Required FFmpeg source material is missing: $material"
    }
}
if (!(Test-Path -LiteralPath $RustNotices -PathType Container)) {
    throw "Rust dependency notices are missing: $RustNotices"
}

New-Item -ItemType Directory -Path $Dist -Force | Out-Null
$packageDir = Join-Path $Dist $Name
$archivePath = Join-Path $Dist ($Name + '.zip')
if ((Test-Path -LiteralPath $packageDir) -or (Test-Path -LiteralPath $archivePath)) {
    throw "Package output already exists. Choose another -Dist or -Name: $packageDir"
}

$appBin = Join-Path $packageDir 'bin'
$codecBin = Join-Path $packageDir 'lib\slicer\bin'
$sourceOut = Join-Path $packageDir 'share\slicer\ffmpeg-source'
$noticeOut = Join-Path $packageDir 'share\slicer'
$rustOut = Join-Path $noticeOut 'rust-notices'
New-Item -ItemType Directory -Path $appBin, $codecBin, $sourceOut, $rustOut -Force | Out-Null
Copy-Item -LiteralPath $Binary -Destination (Join-Path $appBin 'slicer.exe')
Copy-Item -LiteralPath $ffmpeg -Destination (Join-Path $codecBin 'ffmpeg.exe')
Copy-Item -LiteralPath $ffprobe -Destination (Join-Path $codecBin 'ffprobe.exe')

foreach ($item in Get-ChildItem -LiteralPath (Join-Path $FfmpegBundle 'source')) {
    Copy-Item -LiteralPath $item.FullName -Destination $sourceOut -Recurse -Force
}
Copy-Item -LiteralPath (Join-Path $FfmpegBundle 'COPYING.LGPLv2.1') -Destination $noticeOut
Copy-Item -LiteralPath (Join-Path $FfmpegBundle 'FFMPEG-NOTICE.txt') -Destination $noticeOut
$zlibNotice = Join-Path $FfmpegBundle 'ZLIB-NOTICE.txt'
if (Test-Path -LiteralPath $zlibNotice -PathType Leaf) {
    Copy-Item -LiteralPath $zlibNotice -Destination $noticeOut
}
foreach ($item in Get-ChildItem -LiteralPath $RustNotices) {
    Copy-Item -LiteralPath $item.FullName -Destination $rustOut -Recurse -Force
}
Copy-Item -LiteralPath (Join-Path $Root 'docs\packaging.md') -Destination $noticeOut
$readme = Join-Path $Root 'README.md'
if (Test-Path -LiteralPath $readme -PathType Leaf) {
    Copy-Item -LiteralPath $readme -Destination $packageDir
}

# Make the manifest UTF-8 without a PowerShell-version-dependent BOM. The
# manifest is generated before the archive so it does not hash itself.
$manifestPath = Join-Path $packageDir 'SHA256SUMS'
$manifestLines = New-Object 'System.Collections.Generic.List[string]'
$manifestLines.Add('Slicer Windows package manifest') | Out-Null
$manifestLines.Add(('Package:           {0}' -f $Name)) | Out-Null
$manifestLines.Add(('FFmpeg version:    {0}' -f $FfmpegVersion)) | Out-Null
$manifestLines.Add(('FFmpeg source SHA: {0}' -f $SourceSha256)) | Out-Null
$manifestLines.Add('') | Out-Null
$manifestLines.Add('Files (SHA-256):') | Out-Null
foreach ($item in (Get-ChildItem -LiteralPath $packageDir -File -Recurse | Sort-Object FullName)) {
    $relative = $item.FullName.Substring($packageDir.Length + 1).Replace('\', '/')
    $hash = (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    $manifestLines.Add(('{0}  {1}' -f $hash, $relative)) | Out-Null
}
$utf8 = New-Object -TypeName System.Text.UTF8Encoding -ArgumentList $false
[System.IO.File]::WriteAllLines($manifestPath, $manifestLines, $utf8)

Compress-Archive -LiteralPath $packageDir -DestinationPath $archivePath
Write-Output "Windows package ready:"
Write-Output "  directory: $packageDir"
Write-Output "  archive:   $archivePath"
