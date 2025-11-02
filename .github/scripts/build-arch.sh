#!/bin/bash
set -e

# Get version from git tag or Cargo.toml
if [ -n "$GITHUB_REF" ] && [[ "$GITHUB_REF" =~ refs/tags/v ]]; then
    VERSION=$(echo "$GITHUB_REF" | sed 's/refs\/tags\/v//')
else
    VERSION=$(grep -m1 '^version' winetricks-cli/Cargo.toml | cut -d'"' -f2 || grep -m1 '^version' Cargo.toml | cut -d'"' -f2 || echo "0.1.0")
fi

# Update PKGBUILD with version
sed -i "s/^pkgver=.*/pkgver=${VERSION}/" PKGBUILD || echo "PKGBUILD update skipped"

# Start Docker service
sudo systemctl start docker || true

# Build in Docker (Arch Linux) - run as builder user to avoid root restriction
# Note: We can't chown mounted volumes, so we copy files to a directory owned by builder
docker run --rm \
    -v "$(pwd)":/build:ro \
    -e PKGEXT=".pkg.tar.zst" \
    archlinux:latest \
    bash -c "
        pacman -Syu --noconfirm rust cargo openssl base-devel
        useradd -m -s /bin/bash builder
        mkdir -p /home/builder/build
        cp -r /build/* /home/builder/build/ 2>/dev/null || cp -r /build/. /home/builder/build/ 2>/dev/null || true
        chown -R builder:builder /home/builder/build
        su builder -c 'cd /home/builder/build && makepkg -s --noconfirm --skipinteg'
        cp /home/builder/build/*.pkg.tar.zst /build/ 2>/dev/null || true
    " || {
    echo "Docker build failed, trying with podman..."
    podman run --rm \
        -v "$(pwd)":/build:ro \
        archlinux:latest \
        bash -c "
            pacman -Syu --noconfirm rust cargo openssl base-devel
            useradd -m -s /bin/bash builder
            mkdir -p /home/builder/build
            cp -r /build/* /home/builder/build/ 2>/dev/null || cp -r /build/. /home/builder/build/ 2>/dev/null || true
            chown -R builder:builder /home/builder/build
            su builder -c 'cd /home/builder/build && makepkg -s --noconfirm --skipinteg'
            cp /home/builder/build/*.pkg.tar.zst /build/ 2>/dev/null || true
        "
}

# Find and copy the package
PKG=$(ls -t winetricks-*.pkg.tar.zst 2>/dev/null | head -1)
if [ -n "$PKG" ]; then
    echo "Package created: $PKG"
    cp "$PKG" ./ || true
else
    echo "Warning: Package file not found"
    ls -la *.pkg.tar.* 2>/dev/null || true
fi

