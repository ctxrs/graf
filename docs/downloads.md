# Release downloads

Graf releases provide these standalone executables:

| Platform | Asset | Requirement |
| --- | --- | --- |
| Linux x64 | `graf-linux-x64` | glibc 2.28 or newer |
| Linux ARM64 | `graf-linux-aarch64` | glibc 2.28 or newer |
| macOS Intel | `graf-macos-x64` | macOS 13 or newer |
| macOS Apple Silicon | `graf-macos-arm64` | macOS 13 or newer |
| Windows x64 | `graf-windows-x64.exe` | 64-bit Windows |

Get the executable for your platform from the same tagged
[GitHub release](https://github.com/ctxrs/graf/releases) as its verification files.
Each executable also has a CycloneDX software bill of materials (`.cdx.json`)
and third-party license notices (`.third-party-notices.txt`).

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
Compare your download's SHA-256 with its entry in the verified manifest.

On Linux:

```sh
sha256sum graf-linux-x64
```

On macOS:

```sh
shasum -a 256 graf-macos-arm64
```

On Windows PowerShell:

```powershell
Get-FileHash .\graf-windows-x64.exe -Algorithm SHA256
Get-AuthenticodeSignature .\graf-windows-x64.exe
```

The Windows signature must be `Valid` and identify `CTX ENGINEERING, INC.`.
macOS executables are Developer ID signed and notarized by Apple. `SHA256SUMS`
is also provided for checking the complete downloaded asset set; the signed
manifest authenticates those hashes.

## Install

After verification, install the downloaded Unix executable as `graf` in a
directory on your `PATH`. For example, on Linux x64:

```sh
mkdir -p "$HOME/.local/bin"
install -m 0755 graf-linux-x64 "$HOME/.local/bin/graf"
graf --version
```

On Windows, rename `graf-windows-x64.exe` to `graf.exe` and place it in a directory
on your `PATH`. These are manual installations. To upgrade, verify and replace
the executable from a newer release. Existing graph indexes remain local;
`graf update` explicitly refreshes indexed source.
