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
source=("$pkgname-$pkgver.tar.gz")
sha256sums=('SKIP')

build() {
    cd "$srcdir/$pkgname-$pkgver"
    cargo build --release --bin winetricks
    cargo build --release --bin winetricks-gui
}

package() {
    cd "$srcdir/$pkgname-$pkgver"
    install -Dm755 target/release/winetricks "$pkgdir/usr/bin/winetricks"
    install -Dm755 target/release/winetricks-gui "$pkgdir/usr/bin/winetricks-gui"
}

