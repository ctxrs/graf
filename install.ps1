# Install the latest stable Graf release, or set GRAF_VERSION and GRAF_INSTALL_DIR.
# Works in Windows PowerShell 5.1, including: irm <installer-url> | iex
function Install-Graf {
    $ErrorActionPreference = 'Stop'
    Set-StrictMode -Version 2.0

    function Get-GrafDownload([string] $Url, [string] $Path) {
        if ($Url -cnotmatch '\Ahttps://github\.com/ctxrs/graf/releases/(?:latest/download/graf-release\.json|download/v[0-9]+\.[0-9]+\.[0-9]+/(?:graf-release\.json(?:\.sig)?|graf-windows-x64\.exe(?:\.third-party-notices\.txt)?))\z') {
            throw 'Invalid Graf release URL.'
        }
        # Follow GitHub asset redirects explicitly so a redirect cannot downgrade HTTPS.
        $uri = [Uri] $Url
        for ($hop = 0; $hop -lt 10; $hop++) {
            if ($uri.Scheme -cne 'https' -or $uri.UserInfo -or $uri.Port -ne 443 -or
                $uri.DnsSafeHost -cnotin @('github.com', 'release-assets.githubusercontent.com', 'objects.githubusercontent.com')) {
                throw 'Invalid Graf download redirect.'
            }
            $request = [Net.HttpWebRequest]::Create($uri)
            $request.AllowAutoRedirect = $false
            $request.UserAgent = 'Graf-installer'
            $request.Timeout = 60000
            $request.ReadWriteTimeout = 60000
            $response = $request.GetResponse()
            try {
                $status = [int] $response.StatusCode
                if ($status -in @(301, 302, 303, 307, 308)) {
                    if (-not $response.Headers['Location']) { throw 'Missing download redirect.' }
                    $uri = [Uri]::new($uri, $response.Headers['Location'])
                    continue
                }
                if ($status -ne 200) { throw "Unexpected download status: $status" }
                $output = [IO.File]::Open($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
                try { $response.GetResponseStream().CopyTo($output) }
                finally { $output.Dispose() }
                return
            }
            finally { $response.Dispose() }
        }
        throw 'Too many Graf download redirects.'
    }

    function Assert-GrafManifestSignature([byte[]] $Bytes, [string] $SignaturePath) {
        # RSA public key from release-key.pem; RSAParameters also works on .NET Framework.
        $parameters = New-Object Security.Cryptography.RSAParameters
        $parameters.Modulus = [Convert]::FromBase64String('yBPNIx3H/NwWlN9CPHY5kOEe9kQEshOJEMpv3Atq086H1FWqliTm3BCWiO4s/89wNMn11Pla2JetCWNiWsbxm3BIxCd1o6cq8y9ur6Zk1RGOQBLQgqhFm5BpcTTavhtlc3FdV2KSm2UU1IEJAiFXJyMlbgmf3tXfO8Cji/3mG11rWCXfnEzXJmig5/WWA21ZgsafPJGH9ow7FsLok5G1kvOeVDXcv0gzmxWH+2O40kCGWo7BK7P/2DPD2GbXc81Mf6S7vWi7CeFiBeGH8EGZ6MgBM0UnAFEqtx/WvY47O+LHzFrGlJTpss3xlxsSQOTmXDJdOzmQVi04GkbOtBEl+dIyYsxZGusLBMGDqkZekO4Z5LvqA8zHt4JAElZCs8SGTlV70MSlnyZb5/rkKx9kMvb7YjuYbY6vnN5Pp3P7gMhOKehP+62U80cgyj1m6Sk5bByrs54ne2mM+cwNXXgKp5UntmkefDcfKP7MmISy93U/kg3fWojE/a+X6TNV/k5f')
        $parameters.Exponent = [Convert]::FromBase64String('AQAB')
        $rsa = [Security.Cryptography.RSA]::Create()
        try {
            $rsa.ImportParameters($parameters)
            $signature = [Convert]::FromBase64String([IO.File]::ReadAllText($SignaturePath))
            if (-not $rsa.VerifyData($Bytes, $signature, [Security.Cryptography.HashAlgorithmName]::SHA256,
                    [Security.Cryptography.RSASignaturePadding]::Pkcs1)) {
                throw 'Graf release manifest signature is invalid.'
            }
        }
        finally { $rsa.Dispose() }
    }

    function Assert-GrafFileTarget([string] $Path) {
        # GetAttributes catches dangling links too; File.Exists alone does not.
        try { $attributes = [IO.File]::GetAttributes($Path) }
        catch [IO.FileNotFoundException] { return }
        catch [IO.DirectoryNotFoundException] { return }
        if (($attributes -band [IO.FileAttributes]::Directory) -or
            ($attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "Install target is a directory or reparse point: $Path"
        }
    }

    function Move-GrafFile([string] $Source, [string] $Destination) {
        Assert-GrafFileTarget $Destination
        if ([IO.File]::Exists($Destination)) {
            [IO.File]::Replace($Source, $Destination, [Management.Automation.Language.NullString]::Value)
        }
        else { [IO.File]::Move($Source, $Destination) }
    }

    function Get-GrafArtifactHash($Manifest, [string] $Name) {
        $entries = @($Manifest.artifacts | Where-Object { $_.name -is [string] -and $_.name -ceq $Name })
        if ($entries.Count -ne 1 -or $entries[0].sha256 -isnot [string] -or
            $entries[0].sha256 -cnotmatch '\A[0-9a-fA-F]{64}\z') {
            throw "Missing, duplicate, or invalid release artifact: $Name"
        }
        return $entries[0].sha256
    }

    function Assert-GrafHash([string] $Path, [string] $Expected) {
        if ((Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash -ine $Expected) {
            throw "Graf download hash mismatch: $([IO.Path]::GetFileName($Path))"
        }
    }

    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) { throw 'Graf requires native Windows x64.' }
    $nativeArchitecture = $env:PROCESSOR_ARCHITECTURE
    if ($env:PROCESSOR_ARCHITEW6432) { $nativeArchitecture = $env:PROCESSOR_ARCHITEW6432 }
    if ($nativeArchitecture -ine 'AMD64') { throw 'Graf requires native Windows x64 (ARM emulation is unsupported).' }

    $version = $env:GRAF_VERSION
    if ($version) {
        if ($version -cnotmatch '\Av?(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\z') {
            throw 'GRAF_VERSION must be a stable x.y.z version, optionally prefixed with v.'
        }
        $version = $version -creplace '\Av', ''
    }
    $installDir = $env:GRAF_INSTALL_DIR
    if (-not $installDir) {
        $localAppData = [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)
        if (-not $localAppData) { throw 'Cannot locate LocalAppData; set GRAF_INSTALL_DIR.' }
        $installDir = Join-Path $localAppData 'Graf\bin'
    }
    $installDir = [IO.Path]::GetFullPath($installDir)
    $destination = Join-Path $installDir 'graf.exe'
    $noticesDestination = Join-Path $installDir 'graf.third-party-notices.txt'
    Assert-GrafFileTarget $destination
    Assert-GrafFileTarget $noticesDestination
    [void] [IO.Directory]::CreateDirectory($installDir)
    $stage = Join-Path $installDir ('.graf-install-' + [Guid]::NewGuid().ToString('N'))
    [void] [IO.Directory]::CreateDirectory($stage)
    $oldProtocol = [Net.ServicePointManager]::SecurityProtocol
    try {
        [Net.ServicePointManager]::SecurityProtocol = $oldProtocol -bor [Net.SecurityProtocolType]::Tls12
        $releases = 'https://github.com/ctxrs/graf/releases'
        $manifestUrl = "$releases/latest/download/graf-release.json"
        if ($version) { $manifestUrl = "$releases/download/v$version/graf-release.json" }
        $manifestPath = Join-Path $stage 'graf-release.json'
        Get-GrafDownload $manifestUrl $manifestPath
        $bytes = [IO.File]::ReadAllBytes($manifestPath)
        $utf8 = New-Object Text.UTF8Encoding($false, $true)
        $manifest = $utf8.GetString($bytes) | ConvertFrom-Json
        # Only the strictly checked version is used before authenticating these bytes.
        if ($manifest.version -isnot [string] -or
            $manifest.version -cnotmatch '\A(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\z') {
            throw 'Invalid Graf manifest version.'
        }
        $releaseVersion = $manifest.version
        if ($version -and $releaseVersion -cne $version) { throw 'Graf manifest version does not match GRAF_VERSION.' }
        $release = "$releases/download/v$releaseVersion"
        $signaturePath = Join-Path $stage 'graf-release.json.sig'
        Get-GrafDownload "$release/graf-release.json.sig" $signaturePath
        Assert-GrafManifestSignature $bytes $signaturePath
        if ($manifest.product -isnot [string] -or $manifest.product -cne 'graf' -or
            $manifest.repository -isnot [string] -or $manifest.repository -cne 'https://github.com/ctxrs/graf' -or
            ($manifest.schema_version -isnot [int] -and $manifest.schema_version -isnot [long]) -or
            $manifest.schema_version -ne 1 -or $manifest.artifacts -isnot [array] -or $manifest.targets -isnot [array]) { throw 'Invalid Graf release manifest identity or schema.' }
        $binaryName = 'graf-windows-x64.exe'
        $noticesName = "$binaryName.third-party-notices.txt"
        $binaryHash = Get-GrafArtifactHash $manifest $binaryName
        $noticesHash = Get-GrafArtifactHash $manifest $noticesName
        $targets = @($manifest.targets | Where-Object { $_.id -is [string] -and $_.id -ceq 'windows-x64' })
        if ($targets.Count -ne 1 -or $targets[0].artifact -isnot [string] -or
            $targets[0].artifact -cne $binaryName -or $targets[0].sha256 -isnot [string] -or
            $targets[0].sha256 -ine $binaryHash) { throw 'Invalid Graf Windows target.' }
        $binaryPath = Join-Path $stage $binaryName
        $noticesPath = Join-Path $stage $noticesName
        Get-GrafDownload "$release/$binaryName" $binaryPath
        Get-GrafDownload "$release/$noticesName" $noticesPath
        Assert-GrafHash $binaryPath $binaryHash
        Assert-GrafHash $noticesPath $noticesHash
        $authenticode = Get-AuthenticodeSignature -LiteralPath $binaryPath
        if ($authenticode.Status -ne 'Valid' -or $null -eq $authenticode.SignerCertificate -or
            $null -eq $authenticode.TimeStamperCertificate -or
            $authenticode.SignerCertificate.GetNameInfo([Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false) -cne 'CTX ENGINEERING, INC.') {
            throw 'Graf requires a trusted, timestamped Authenticode signature from CTX ENGINEERING, INC.'
        }
        # Execute only after both hashes and the trusted publisher signature pass.
        $versionOutput = @(& $binaryPath --version 2>&1)
        if ($LASTEXITCODE -ne 0 -or $versionOutput.Count -ne 1 -or
            $versionOutput[0] -cne "graf-cli $releaseVersion") {
            throw 'Graf executable version does not match the signed release manifest.'
        }
        # All validation finishes before either destination is changed.
        Assert-GrafFileTarget $destination
        Move-GrafFile $noticesPath $noticesDestination
        Move-GrafFile $binaryPath $destination
        Write-Host "Installed Graf $releaseVersion at $destination"
        Write-Host "Add $installDir to your user PATH to run graf by name. PATH was not changed."
    }
    finally {
        [Net.ServicePointManager]::SecurityProtocol = $oldProtocol
        if ([IO.Directory]::Exists($stage)) { [IO.Directory]::Delete($stage, $true) }
    }
}

Install-Graf
