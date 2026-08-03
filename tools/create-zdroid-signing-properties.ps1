param(
    [string]$StoreFile = (Join-Path $env:USERPROFILE ".zdroid-signing\zdroid-release.jks"),
    [string]$KeyAlias = "zdroid",
    [string]$ProjectRoot = (Resolve-Path "$PSScriptRoot/..").Path
)

$ErrorActionPreference = "Stop"

function Convert-SecureStringToPlainText {
    param([Parameter(Mandatory = $true)][Security.SecureString]$SecureString)

    $bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($SecureString)
    try {
        [Runtime.InteropServices.Marshal]::PtrToStringBSTR($bstr)
    } finally {
        if ($bstr -ne [IntPtr]::Zero) {
            [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr)
        }
    }
}

$appDir = Join-Path $ProjectRoot "crates/gpui_android/examples/zed_android/android/app"
$propertiesPath = Join-Path $appDir "signing.properties"

if (!(Test-Path $StoreFile)) {
    throw "Keystore not found: $StoreFile"
}

if (!(Test-Path $appDir)) {
    throw "Android app directory not found: $appDir"
}

Write-Host "Creating local release signing config:"
Write-Host "  $propertiesPath"
Write-Host ""
Write-Host "Passwords stay on this machine and this file is gitignored."
Write-Host ""

$storePasswordSecure = Read-Host "Keystore password" -AsSecureString
$keyPasswordSecure = Read-Host "Key password (press Enter if same as keystore password)" -AsSecureString

$storePassword = Convert-SecureStringToPlainText $storePasswordSecure
$keyPassword = Convert-SecureStringToPlainText $keyPasswordSecure

if ([string]::IsNullOrEmpty($keyPassword)) {
    $keyPassword = $storePassword
}

$content = @"
storeFile=$StoreFile
storePassword=$storePassword
keyAlias=$KeyAlias
keyPassword=$keyPassword
"@

Set-Content -LiteralPath $propertiesPath -Value $content -NoNewline -Encoding ASCII

# signing.properties contains plaintext passwords because that is the format
# consumed by Gradle. Restrict it to the current Windows account in addition
# to keeping it out of Git.
$currentIdentity = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
& icacls.exe $propertiesPath /inheritance:r /grant:r "${currentIdentity}:(F)" | Out-Null
if ($LASTEXITCODE -ne 0) {
    throw "Failed to restrict permissions on $propertiesPath"
}

$content = $null
$storePassword = $null
$keyPassword = $null

Write-Host ""
Write-Host "Wrote $propertiesPath"
Write-Host "This file is intentionally ignored by git."
