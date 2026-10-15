#!/bin/bash
# The signing certificate in CI (part 10.8): releases are signed with the same self-signed "Flux
# Dev" identity as the author's builds, so the Keychain keeps «Always Allow» of plugin secrets
# across updates. The identity comes from the repository's secrets into a temporary keychain that
# lives only as long as the job.
#
#   MACOS_CERT_P12=<base64 of the .p12> MACOS_CERT_PASSWORD=<its password> \
#       scripts/ci-keychain.sh import [keychain]
#   scripts/ci-keychain.sh delete [keychain]
#
# The keychain defaults to $RUNNER_TEMP/flux-signing.keychain-db (or $TMPDIR). `import` creates and
# unlocks it with a random password, imports the identity for codesign only and checks that the
# identity can sign. The user's keychain search list isn't touched: scripts/bundle-macos.sh looks in
# this keychain alone when FLUX_SIGN_KEYCHAIN names it (`codesign --keychain`). The certificate is
# self-signed and not trusted — codesign signs with it all the same; only `find-identity -v` (valid
# identities) leaves it out. `delete` deletes the keychain: run it in an always() step.
set -euo pipefail

ACTION=${1:-}
KEYCHAIN=${2:-${RUNNER_TEMP:-${TMPDIR:-/tmp}}/flux-signing.keychain-db}
IDENTITY=${FLUX_SIGN_IDENTITY:-Flux Dev}

case "$ACTION" in
    import)
        : "${MACOS_CERT_P12:?MACOS_CERT_P12 (base64 of the .p12) is needed}"
        : "${MACOS_CERT_PASSWORD?MACOS_CERT_PASSWORD is needed (may be empty)}"
        p12=$(mktemp "${TMPDIR:-/tmp}/flux-cert.XXXXXX")
        trap 'rm -f "$p12"' EXIT
        printf '%s' "$MACOS_CERT_P12" | base64 --decode >"$p12"
        password=$(openssl rand -hex 24)

        security delete-keychain "$KEYCHAIN" 2>/dev/null || true
        security create-keychain -p "$password" "$KEYCHAIN"
        # No locking after a timeout or on sleep while the job runs.
        security set-keychain-settings "$KEYCHAIN"
        security unlock-keychain -p "$password" "$KEYCHAIN"
        # On CI's runner (an ephemeral machine) the keychain also joins the search list: macOS 15's
        # import of a private key into a keychain outside it fails with "The specified item could
        # not be found in the keychain". Locally the user's list stays as it is.
        if [ -n "${CI:-}" ]; then
            # shellcheck disable=SC2046
            security list-keychains -d user -s "$KEYCHAIN" $(security list-keychains -d user | tr -d '"')
            security list-keychains -d user
        fi
        security import "$p12" -k "$KEYCHAIN" -f pkcs12 -P "$MACOS_CERT_PASSWORD" \
            -T /usr/bin/codesign -T /usr/bin/security
        # codesign may use the key without asking (there is no one to click «Allow» in CI).
        security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$password" \
            "$KEYCHAIN" >/dev/null
        if ! security find-identity -p codesigning "$KEYCHAIN" | grep -qF "\"$IDENTITY\""; then
            echo "error: the .p12 holds no \"$IDENTITY\" code signing identity:" >&2
            security find-identity -p codesigning "$KEYCHAIN" >&2
            exit 1
        fi
        # A signature proves the key is usable without a prompt.
        probe=$(mktemp "${TMPDIR:-/tmp}/flux-sign-probe.XXXXXX")
        cp /usr/bin/true "$probe"
        codesign --force --sign "$IDENTITY" --keychain "$KEYCHAIN" --timestamp=none "$probe"
        rm -f "$probe"
        echo "\"$IDENTITY\" is ready to sign: FLUX_SIGN_KEYCHAIN=$KEYCHAIN"
        ;;
    delete)
        security delete-keychain "$KEYCHAIN" 2>/dev/null || true
        echo "deleted $KEYCHAIN"
        ;;
    *)
        echo "usage: $0 import|delete [keychain]" >&2
        exit 2
        ;;
esac
