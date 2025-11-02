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
    install -Dm755 target/release/winetricks "$pkgdir/usr/bin/winetricks"
    install -Dm755 target/release/winetricks-gui "$pkgdir/usr/bin/winetricks-gui"
}

