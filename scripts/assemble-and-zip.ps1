# 组装并打包 4 个便携版发行版（联网 x64/x86 + 自包含 x64/x86）
# 产物：dist\PDFReader-联网版-x64.zip 等 4 个 zip
$ErrorActionPreference = 'Stop'
$root = 'D:\副业\code\PDFReader\PDFReader'
$dist = Join-Path $root 'dist'
$sidecar = 'D:\副业\code\PDFReader\pdfomml\rust\pdfomml\sidecar\pdfomml'
$x64exe = Join-Path $root 'target\release\pdfreader.exe'
$x86exe = Join-Path $root 'target\i686-pc-windows-msvc\release\pdfreader.exe'
$wv2x64 = Join-Path $dist 'downloads\WebView2Setup-x64.exe'
$wv2x86 = Join-Path $dist 'downloads\WebView2Setup-x86.exe'

function New-EmptyDir($path) {
    if (Test-Path $path) { Remove-Item $path -Recurse -Force -Confirm:$false }
    New-Item -ItemType Directory -Force $path | Out-Null
}
function Mirror-Sidecar($destDir) {
    $null = robocopy $sidecar $destDir /MIR /NFL /NDL /NJH /NJS /NP
    if ($LASTEXITCODE -ge 8) { throw "robocopy 失败: $destDir ($LASTEXITCODE)" }
}

Write-Host "[1/4] 联网版 x64"
$d = Join-Path $dist 'PDFReader-联网版-x64'
New-EmptyDir $d
Copy-Item $x64exe (Join-Path $d 'PDFReader.exe')
Mirror-Sidecar (Join-Path $d 'pdfomml')

Write-Host "[2/4] 联网版 x86"
$d = Join-Path $dist 'PDFReader-联网版-x86'
New-EmptyDir $d
Copy-Item $x86exe (Join-Path $d 'PDFReader.exe')

Write-Host "[3/4] 自包含版 x64"
$d = Join-Path $dist 'PDFReader-自包含版-x64'
New-EmptyDir $d
Copy-Item $x64exe (Join-Path $d 'PDFReader.exe')
Copy-Item $wv2x64 (Join-Path $d 'WebView2Setup.exe')
Mirror-Sidecar (Join-Path $d 'pdfomml')

Write-Host "[4/4] 自包含版 x86"
$d = Join-Path $dist 'PDFReader-自包含版-x86'
New-EmptyDir $d
Copy-Item $x86exe (Join-Path $d 'PDFReader.exe')
Copy-Item $wv2x86 (Join-Path $d 'WebView2Setup.exe')

Write-Host "组装完成，开始打包 zip ..."
foreach ($name in @('PDFReader-联网版-x64','PDFReader-联网版-x86','PDFReader-自包含版-x64','PDFReader-自包含版-x86')) {
    $src = Join-Path $dist $name
    $zip = Join-Path $dist ($name + '.zip')
    if (Test-Path $zip) { Remove-Item $zip -Force -Confirm:$false }
    Write-Host "  压缩 $name ..."
    $ok = $false
    for ($attempt = 1; $attempt -le 4 -and -not $ok; $attempt++) {
        try {
            Compress-Archive -Path $src -DestinationPath $zip -CompressionLevel Optimal -ErrorAction Stop
            $ok = $true
        } catch {
            if ($attempt -lt 4) {
                Write-Host "    第 $attempt 次失败（$($_.Exception.Message)），5 秒后重试 ..."
                Start-Sleep -Seconds 5
            } else {
                throw
            }
        }
    }
    $mb = [math]::Round((Get-Item $zip).Length / 1MB, 1)
    Write-Host "    -> $name.zip ($mb MB)"
}
Write-Host "全部完成"
