#!/bin/sh
# Install a verified Graf release. Run with sh; no administrator access needed.

main() (
    set -eu
    fail() { printf 'graf installer: %s\n' "$*" >&2; exit 1; }
    version=${GRAF_VERSION:-}
    install_dir=${GRAF_INSTALL_DIR:-}
    from=${GRAF_FROM:-}
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --version|--install-dir|--from)
                [ "$#" -ge 2 ] || fail "$1 requires a value"
                [ -n "$2" ] || fail "$1 requires a nonempty value"
                case "$1" in --version) version=$2 ;; --install-dir) install_dir=$2 ;; --from) from=$2 ;; esac
                shift 2 ;;
            -h|--help)
                printf '%s\n' 'Install Graf: sh install.sh [--version 0.2.0] [--install-dir DIR] [--from graphify]' \
                    'Defaults: latest release, $HOME/.local/bin.' \
                    'Environment: GRAF_VERSION, GRAF_INSTALL_DIR, GRAF_FROM. Shell profiles are not changed.' \
                    '--from graphify runs Graf migration from your current directory after installation (requires Graf 0.2+).'
                exit 0 ;;
            *) fail "unknown argument: $1" ;;
        esac
    done
    case "$from" in ''|graphify) ;; *) fail 'GRAF_FROM/--from must be graphify' ;; esac
    valid_version() {
        case "$1" in ''|*[!0-9.]*) return 1 ;; esac
        printf '%s\n' "$1" | LC_ALL=C grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'
    }
    if [ -n "$version" ]; then
        version=${version#v}
        valid_version "$version" || fail 'version must have the form 0.1.0'
    fi
    requested_version=$version
    for tool in curl openssl awk grep uname mktemp; do
        command -v "$tool" >/dev/null 2>&1 || fail "required command not found: $tool"
    done
    system=$(uname -s)
    machine=$(uname -m)
    case "$system" in
        Darwin)
            # Select the native ARM binary even when invoked from a Rosetta shell.
            if [ "$(/usr/sbin/sysctl -in hw.optional.arm64 2>/dev/null || true)" = 1 ]; then
                machine=arm64
            fi ;;
    esac
    case "$system/$machine" in
        Linux/x86_64) artifact=graf-linux-x64 ;;
        Linux/aarch64|Linux/arm64) artifact=graf-linux-aarch64 ;;
        Darwin/x86_64) artifact=graf-macos-x64 ;;
        Darwin/arm64) artifact=graf-macos-arm64 ;;
        *) fail "unsupported platform: $system/$machine" ;;
    esac
    if [ -z "$install_dir" ]; then
        [ -n "${HOME:-}" ] || fail 'set HOME or GRAF_INSTALL_DIR'
        install_dir=$HOME/.local/bin
    fi
    case "$install_dir" in /*) ;; *) install_dir=$(pwd)/$install_dir ;; esac
    umask 077
    mkdir -p "$install_dir"
    for name in graf graf.third-party-notices.txt; do
        [ ! -L "$install_dir/$name" ] || fail "refusing to replace a symlink: $install_dir/$name"
        [ ! -e "$install_dir/$name" ] || [ -f "$install_dir/$name" ] || fail "not a regular file: $install_dir/$name"
    done
    graf_stage=$(mktemp -d "$install_dir/.graf-install.XXXXXX")
    trap 'rm -rf "$graf_stage"' 0
    trap 'exit 130' INT
    trap 'exit 143' TERM
    download() {
        curl --proto '=https' --proto-redir '=https' --tlsv1.2 -fsSL \
            --retry 3 --connect-timeout 15 --max-time 300 "$1" -o "$2" || fail 'download failed'
    }
    release_root=https://github.com/ctxrs/graf/releases
    if [ -n "$version" ]; then
        manifest_url=$release_root/download/v$version/graf-release.json
    else
        manifest_url=$release_root/latest/download/graf-release.json
    fi
    printf 'Downloading Graf release metadata...\n'
    download "$manifest_url" "$graf_stage/manifest.json"
    # The signed release format is canonical, indented JSON emitted by Graf.
    # Read only exact field lines; never evaluate metadata as shell input.
    field() {
        awk -F '"' -v key="$1" '
            $0 ~ "^  \"" key "\": \"[^\"]+\",?$" { count++; value=$4 }
            END { if (count != 1) exit 1; print value }
        ' "$graf_stage/manifest.json"
    }
    version=$(field version) || fail 'missing or ambiguous release version'
    valid_version "$version" || fail 'invalid release version'
    [ -z "$requested_version" ] || [ "$version" = "$requested_version" ] || fail 'release version mismatch'
    release_url=$release_root/download/v$version
    download "$release_url/graf-release.json.sig" "$graf_stage/manifest.sig"
    cat > "$graf_stage/key.pem" <<'GRAF_RELEASE_KEY'
-----BEGIN PUBLIC KEY-----
MIIBojANBgkqhkiG9w0BAQEFAAOCAY8AMIIBigKCAYEAyBPNIx3H/NwWlN9CPHY5
kOEe9kQEshOJEMpv3Atq086H1FWqliTm3BCWiO4s/89wNMn11Pla2JetCWNiWsbx
m3BIxCd1o6cq8y9ur6Zk1RGOQBLQgqhFm5BpcTTavhtlc3FdV2KSm2UU1IEJAiFX
JyMlbgmf3tXfO8Cji/3mG11rWCXfnEzXJmig5/WWA21ZgsafPJGH9ow7FsLok5G1
kvOeVDXcv0gzmxWH+2O40kCGWo7BK7P/2DPD2GbXc81Mf6S7vWi7CeFiBeGH8EGZ
6MgBM0UnAFEqtx/WvY47O+LHzFrGlJTpss3xlxsSQOTmXDJdOzmQVi04GkbOtBEl
+dIyYsxZGusLBMGDqkZekO4Z5LvqA8zHt4JAElZCs8SGTlV70MSlnyZb5/rkKx9k
Mvb7YjuYbY6vnN5Pp3P7gMhOKehP+62U80cgyj1m6Sk5bByrs54ne2mM+cwNXXgK
p5UntmkefDcfKP7MmISy93U/kg3fWojE/a+X6TNV/k5fAgMBAAE=
-----END PUBLIC KEY-----
GRAF_RELEASE_KEY
    openssl enc -A -d -base64 -in "$graf_stage/manifest.sig" -out "$graf_stage/signature" 2>/dev/null \
        || fail 'invalid manifest signature encoding'
    openssl dgst -sha256 -verify "$graf_stage/key.pem" -signature "$graf_stage/signature" \
        "$graf_stage/manifest.json" >/dev/null 2>&1 || fail 'release manifest signature verification failed'
    [ "$(field product)" = graf ] || fail 'unexpected release product'
    [ "$(field repository)" = https://github.com/ctxrs/graf ] || fail 'unexpected release repository'
    [ "$(grep -c '^  "schema_version": 1,$' "$graf_stage/manifest.json")" = 1 ] || fail 'unsupported release schema'
    artifact_hash() {
        awk -F '"' -v name="$1" '
            $0 == "      \"name\": \"" name "\"," {
                count++
                if (getline <= 0 || $0 !~ /^      "sha256": "[0-9a-f]+",$/) bad=1
                value=$4
            }
            END { if (count != 1 || bad || length(value) != 64) exit 1; print value }
        ' "$graf_stage/manifest.json"
    }
    verified_download() {
        expected_hash=$(artifact_hash "$1") || fail "missing or malformed release hash: $1"
        download "$release_url/$1" "$2"
        actual_hash=$(openssl dgst -sha256 "$2") || fail 'could not hash download'
        [ "${actual_hash##* }" = "$expected_hash" ] || fail "download checksum mismatch: $1"
    }
    printf 'Installing Graf %s (%s)...\n' "$version" "$artifact"
    verified_download "$artifact" "$graf_stage/graf"
    verified_download "$artifact.third-party-notices.txt" "$graf_stage/notices"
    chmod 0755 "$graf_stage/graf"
    chmod 0644 "$graf_stage/notices"
    if [ "$system" = Darwin ]; then
        /usr/bin/codesign --verify --strict "$graf_stage/graf" || fail 'macOS code signature verification failed'
    fi
    installed_version=$("$graf_stage/graf" --version) || fail 'Graf could not run; requires glibc 2.28+ on Linux or macOS 13+'
    [ "$installed_version" = "graf-cli $version" ] || fail 'executable version mismatch'
    mv -f "$graf_stage/notices" "$install_dir/graf.third-party-notices.txt"
    mv -f "$graf_stage/graf" "$install_dir/graf"
    printf 'Installed Graf %s to %s/graf\n' "$version" "$install_dir"
    if [ "$from" = graphify ]; then
        "$install_dir/graf" switch graphify || fail \
            'Installation succeeded, but Graphify migration failed. Migration requires Graf 0.2 or later; if pinned to an older release, update GRAF_VERSION/--version and retry. Otherwise, resolve the error above and rerun the installed Graf with: switch graphify'
    fi
    case :${PATH:-}: in
        *:"$install_dir":*) printf 'Run: graf --help\n' ;;
        *) printf 'Add %s to your PATH, then run: graf --help\n' "$install_dir" ;;
    esac
)

main "$@"
