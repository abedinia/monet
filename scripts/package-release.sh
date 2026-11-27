#!/bin/sh
# Package already built release binaries as Debian packages in releases/.
#
#   cargo build --release --locked --target x86_64-unknown-linux-gnu
#   cargo build --release --locked --target aarch64-unknown-linux-gnu
#   ./scripts/package-release.sh
set -eu
umask 022
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)
OUT="$ROOT/releases"
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT HUP INT TERM

package_deb() {
    triple=$1
    arch=$2
    deb_arch=$3
    binary="$TARGET/$triple/release/monet"
    if [ ! -f "$binary" ]; then
        printf '%s\n' "Missing $binary; build the $triple target first." >&2
        exit 1
    fi
    root="$STAGE/$deb_arch"
    mkdir -p "$root/DEBIAN" "$root/usr/bin" "$root/usr/share/doc/monet"
    chmod 0755 "$root"
    install -m 0755 "$binary" "$root/usr/bin/monet"
    install -m 0644 "$ROOT/LICENSE" "$root/usr/share/doc/monet/copyright"
    # The newest glibc symbol the binary needs sets the minimum libc6.
    glibc=$(grep -ao 'GLIBC_[0-9][0-9.]*' "$binary" | sed 's/GLIBC_//' | sort -Vu | tail -n 1)
    cat > "$root/DEBIAN/control" <<EOF
Package: monet
Version: $VERSION
Section: net
Priority: optional
Architecture: $deb_arch
Depends: libc6 (>= $glibc), libgcc-s1, coreutils
Maintainer: Monet contributors
Description: terminal network monitor with live charts
 Live download, upload, connection, latency and event views for Linux.
EOF
    dpkg-deb --build --root-owner-group "$root" "$OUT/monet-$VERSION-linux-$arch.deb" >/dev/null
}

mkdir -p "$OUT"
rm -f "$OUT"/monet-*.deb
package_deb x86_64-unknown-linux-gnu x86_64 amd64
package_deb aarch64-unknown-linux-gnu aarch64 arm64
(cd "$OUT" && sha256sum monet-*.deb > SHA256SUMS)
printf '%s\n' "Wrote Debian packages to $OUT"
