#!/bin/sh
# Make the identity Quipcall.app is signed with: a self-signed certificate in
# a keychain of its own (no password, not in the search list).
#
#   deploy/mac/signing.sh <keychain>
#
# Nobody trusts the certificate, and nothing needs to: macOS remembers Screen
# Recording, camera and microphone permission by the app's signature. An
# ad-hoc signature changes with every build, so every update lost them, while
# System Settings went on showing the old build as allowed. With one
# certificate the signature's requirement stays the same across builds
# (`identifier "com.quipcall.desktop" and certificate leaf = H"…"`).
#
# Back the keychain up. A new certificate means everyone grants permissions
# once more.
set -eu

KEYCHAIN=$1
NAME="Quipcall Signing"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

cat >"$TMP/cert.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $NAME
[ext]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
EOF
openssl req -x509 -newkey rsa:2048 -nodes -days 7300 -config "$TMP/cert.cnf" \
	-keyout "$TMP/key.pem" -out "$TMP/cert.pem" 2>/dev/null
# -legacy: `security import` can't read OpenSSL 3's default PKCS#12 cipher.
openssl pkcs12 -export -legacy -inkey "$TMP/key.pem" -in "$TMP/cert.pem" \
	-out "$TMP/id.p12" -passout pass:quipcall 2>/dev/null ||
	openssl pkcs12 -export -inkey "$TMP/key.pem" -in "$TMP/cert.pem" \
		-out "$TMP/id.p12" -passout pass:quipcall

security create-keychain -p "" "$KEYCHAIN"
chmod 600 "$KEYCHAIN"
security unlock-keychain -p "" "$KEYCHAIN"
security import "$TMP/id.p12" -k "$KEYCHAIN" -P quipcall -T /usr/bin/codesign >/dev/null
# Lets codesign use the key without asking.
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "" "$KEYCHAIN" >/dev/null
echo "created the signing identity \"$NAME\" in $KEYCHAIN; back it up"
