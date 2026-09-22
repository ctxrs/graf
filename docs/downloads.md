# Release downloads

Graf 0.3 and later provide these gzip-compressed standalone executables:

| Platform | Asset | Requirement |
| --- | --- | --- |
| Linux x64 | `graf-linux-x64.gz` | glibc 2.28 or newer |
| Linux ARM64 | `graf-linux-aarch64.gz` | glibc 2.28 or newer |
| macOS Intel | `graf-macos-x64.gz` | macOS 13 or newer |
| macOS Apple Silicon | `graf-macos-arm64.gz` | macOS 13 or newer |
| Windows x64 | `graf-windows-x64.exe.gz` | 64-bit Windows |

Get the executable for your platform from the same tagged
[GitHub release](https://github.com/ctxrs/graf/releases) as its verification files.
Each executable also has a CycloneDX software bill of materials (`.cdx.json`)
and third-party license notices (`.third-party-notices.txt`), named using the
uncompressed executable basename, such as `graf-linux-x64.cdx.json`.
Earlier releases provide raw executables; both installers still support them.

## Verify the download

`graf-release.json` identifies the version, source commit, and SHA-256 digest of
every payload. Its detached signature, `graf-release.json.sig`, is base64-encoded
RSA PKCS#1 v1.5 with SHA-256. Verify it using the trusted
[release public key](../release-key.pem) from this repository:

```sh
openssl base64 -d -in graf-release.json.sig -out graf-release.signature
openssl dgst -sha256 -verify release-key.pem \
  -signature graf-release.signature graf-release.json
```

The command must report `Verified OK`. Check that the manifest names `graf`,
the repository `https://github.com/ctxrs/graf`, and the version you selected.
Compare your download's SHA-256 with its entry in the verified manifest's
`artifacts` array before decompressing it.

On Linux:

```sh
sha256sum graf-linux-x64.gz
```

On macOS:

```sh
shasum -a 256 graf-macos-arm64.gz
```

On Windows PowerShell:

```powershell
Get-FileHash .\graf-windows-x64.exe.gz -Algorithm SHA256
```

Extract the verified file on Unix with `gzip -dk graf-linux-x64.gz`, substituting
your platform's filename. On Windows, use a gzip-capable utility or PowerShell:

```powershell
$inputFile = [IO.File]::OpenRead("$PWD\graf-windows-x64.exe.gz")
try {
    $gzip = [IO.Compression.GZipStream]::new($inputFile, [IO.Compression.CompressionMode]::Decompress)
    try {
        $outputFile = [IO.File]::Open("$PWD\graf-windows-x64.exe", [IO.FileMode]::CreateNew)
        try { $gzip.CopyTo($outputFile) } finally { $outputFile.Dispose() }
    } finally { $gzip.Dispose() }
} finally { $inputFile.Dispose() }
```

Before execution, compare the extracted file's SHA-256 and byte length with
`binary_sha256` and `binary_size` in your platform's `targets` entry of the
authenticated schema-2 manifest. Use `sha256sum` (Linux) or `shasum -a 256`
(macOS) and `wc -c`; on Windows use `Get-FileHash` and `(Get-Item FILE).Length`.
Schema-1 releases are already uncompressed: their artifact hash is the executable
hash, so skip extraction and the schema-2 fields.

Check the extracted Windows executable's native signature:

```powershell
Get-AuthenticodeSignature .\graf-windows-x64.exe
```

The Windows signature must be `Valid` and identify `CTX ENGINEERING, INC.`.
macOS executables are Developer ID signed and notarized by Apple. `SHA256SUMS`
is also provided for checking the complete downloaded asset set; the signed
manifest authenticates those hashes.

## Install with a script

Linux and macOS need `curl`, `openssl`, `gzip`, and standard shell utilities. Windows
needs PowerShell 5.1 or newer. The scripts select the appropriate executable,
verify the signed manifest, download hashes, and extracted executable hash/size,
and install `graf` with its
license notices. The new executable must report the expected version before
replacing an existing installation. macOS code signatures and Windows
Authenticode signatures are checked as well.

```sh
curl -fsSL https://raw.githubusercontent.com/ctxrs/graf/main/install.sh | sh
```

The default directory is `~/.local/bin`. If it is not already on your `PATH`,
add it in your shell configuration; for the current sh/bash/zsh session:

```sh
export PATH="$HOME/.local/bin:$PATH"
graf --version
```

To choose a version or directory:

```sh
curl -fsSL https://raw.githubusercontent.com/ctxrs/graf/main/install.sh \
  | sh -s -- --version 0.6.0 --install-dir "$HOME/bin"
```

On Windows:

```powershell
irm https://raw.githubusercontent.com/ctxrs/graf/main/install.ps1 | iex
```

The default directory is `%LOCALAPPDATA%\Graf\bin`. Add it to your `PATH` in
Windows environment settings if needed. To select a version or another directory,
set `$env:GRAF_VERSION = '0.6.0'` or `$env:GRAF_INSTALL_DIR = 'C:\Tools\Graf'`
before running the command. Those same environment variables work with the
Unix installer. Neither script requires administrator access for its default
directory or modifies your shell profiles.

Rerun the installer to upgrade to the latest release, or set a version to install
that release explicitly. A failed download or verification leaves the existing
executable intact. Graph indexes are separate from the installed executable;
`graf update` refreshes indexed source, not the Graf application.

## Install and switch from Graphify

Run from the Graphify project directory. On Linux or macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/ctxrs/graf/main/install.sh \
  | sh -s -- --from graphify
```

On Windows PowerShell:

```powershell
$env:GRAF_FROM = 'graphify'
try {
    irm https://raw.githubusercontent.com/ctxrs/graf/main/install.ps1 | iex
} finally {
    Remove-Item Env:GRAF_FROM -ErrorAction SilentlyContinue
}
```

The installer first verifies and installs Graf, then runs the installed
executable's `switch graphify` command from your current directory. Graf 0.2 or
later is required. If migration fails, the installed executable remains available
and the installer reports the migration error; resolve it and rerun
`graf switch graphify`. Use Graf directly for `--project`, `--config`, `--graph`,
and `--server` selection. See [migrating from Graphify](migrate-from-graphify.md)
for advanced selection, compatibility, and undo.

## Install manually

After verification and extraction, install the Unix executable as `graf` in a
directory on your `PATH`. For example, on Linux x64:

```sh
mkdir -p "$HOME/.local/bin"
install -m 0755 graf-linux-x64 "$HOME/.local/bin/graf"
graf --version
```

On Windows, rename `graf-windows-x64.exe` to `graf.exe` and place it in a directory
on your `PATH`. To upgrade manually, verify and replace
the executable from a newer release. Existing graph indexes remain local;
`graf update` explicitly refreshes indexed source.
