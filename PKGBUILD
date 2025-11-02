# Maintainer: Winetricks Contributors <winetricks@example.com>
pkgname=winetricks
pkgver=0.1.0
pkgrel=1
pkgdesc="A fast, modern package manager for Wine"
arch=('x86_64')
url="https://github.com/Winetricks/winetricks"
license=('LGPL')
depends=('wine')
makedepends=('rust' 'cargo' 'openssl')
# No source needed - we're using pre-built binaries
source=()
sha256sums=()

build() {
    # No build needed - binaries are pre-built
    return 0
}

package() {
    # Install pre-built binaries
    # $startdir is the directory where makepkg was invoked (e.g., /home/builder/build)
    # We need to use this since we have no source and binaries are in the startdir
    if [ -f "$startdir/target/release/winetricks" ]; then
        install -Dm755 "$startdir/target/release/winetricks" "$pkgdir/usr/bin/winetricks"
        install -Dm755 "$startdir/target/release/winetricks-gui" "$pkgdir/usr/bin/winetricks-gui"
    elif [ -f "target/release/winetricks" ]; then
        # Fallback: try current directory
        install -Dm755 target/release/winetricks "$pkgdir/usr/bin/winetricks"
        install -Dm755 target/release/winetricks-gui "$pkgdir/usr/bin/winetricks-gui"
    else
        # Last resort: search for binaries
        BINARY=$(find "$startdir" -path "*/target/release/winetricks" -type f 2>/dev/null | head -1)
        GUI_BINARY=$(find "$startdir" -path "*/target/release/winetricks-gui" -type f 2>/dev/null | head -1)
        if [ -n "$BINARY" ] && [ -f "$BINARY" ]; then
            install -Dm755 "$BINARY" "$pkgdir/usr/bin/winetricks"
        fi
        if [ -n "$GUI_BINARY" ] && [ -f "$GUI_BINARY" ]; then
            install -Dm755 "$GUI_BINARY" "$pkgdir/usr/bin/winetricks-gui"
        fi
    fi
}

