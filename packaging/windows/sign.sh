#!/bin/sh
# Signe un exécutable Windows (Authenticode) avec osslsigncode, si un certificat
# de signature de code est fourni ; sinon ne fait rien.
#
#   WINDOWS_CERT_PFX       chemin du certificat au format PKCS#12 (.pfx)
#   WINDOWS_CERT_PASSWORD  son mot de passe
#
# Usage : sign.sh fichier.exe
set -eu
file=$1
if [ -z "${WINDOWS_CERT_PFX:-}" ]; then
    echo "sign.sh : aucun certificat (WINDOWS_CERT_PFX), $file n'est pas signé"
    exit 0
fi
command -v osslsigncode >/dev/null || { echo "sign.sh : osslsigncode introuvable"; exit 1; }
pass=$(mktemp)
trap 'rm -f "$pass" "$file.signed"' EXIT
printf '%s' "${WINDOWS_CERT_PASSWORD:-}" > "$pass"
osslsigncode sign -pkcs12 "$WINDOWS_CERT_PFX" -readpass "$pass" -h sha256 \
    -n "Mulot-scan" -i "https://github.com/0ddityspace/mulot-scan" \
    -ts http://timestamp.digicert.com \
    -in "$file" -out "$file.signed"
mv "$file.signed" "$file"
echo "sign.sh : $file signé"
