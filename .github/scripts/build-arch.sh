#!/bin/bash
set -e

# Get version from git tag or workspace Cargo.toml
if [ -n "$GITHUB_REF" ] && [[ "$GITHUB_REF" =~ refs/tags/v ]]; then
    VERSION=$(echo "$GITHUB_REF" | sed 's/refs\/tags\/v//')
else
    # Get version from workspace Cargo.toml (where version is actually defined)
    VERSION=$(grep -m1 '^version\s*=' Cargo.toml | cut -d'"' -f2 | tr -d '[:space:]' || echo "0.1.0")
fi

# Validate version
if [ -z "$VERSION" ] || [ "$VERSION" = "" ]; then
    echo "Error: Could not extract version" >&2
    VERSION="0.1.0"
fi

echo "Building Arch package for version: $VERSION"

# Update PKGBUILD with version
sed -i "s/^pkgver=.*/pkgver=${VERSION}/" PKGBUILD || echo "PKGBUILD update skipped"
# Verify the update worked
if ! grep -q "^pkgver=${VERSION}$" PKGBUILD; then
    echo "Warning: PKGBUILD pkgver update may have failed, current pkgver:"
    grep "^pkgver=" PKGBUILD
fi

# Start Docker service
sudo systemctl start docker || true

# Build in Docker (Arch Linux) - run as builder user to avoid root restriction
# Note: We can't chown mounted volumes, so we copy files to a directory owned by builder
docker run --rm \
    -v "$(pwd)":/build:ro \
    -e PKGEXT=".pkg.tar.zst" \
    archlinux:latest \
    bash -c "
        pacman -Syu --noconfirm rust cargo openssl base-devel wine || echo 'Warning: wine installation failed, continuing anyway'
        useradd -m -s /bin/bash builder
        mkdir -p /home/builder/build
        cp -r /build/* /home/builder/build/ 2>/dev/null || cp -r /build/. /home/builder/build/ 2>/dev/null || true
        chown -R builder:builder /home/builder/build
        su builder -c 'cd /home/builder/build && makepkg --noconfirm --nodeps --skipinteg'
        cp /home/builder/build/*.pkg.tar.zst /build/ 2>/dev/null || true
    " || {
    echo "Docker build failed, trying with podman..."
    podman run --rm \
        -v "$(pwd)":/build:ro \
        archlinux:latest \
        bash -c "
            pacman -Syu --noconfirm rust cargo openssl base-devel wine || echo 'Warning: wine installation failed, continuing anyway'
            useradd -m -s /bin/bash builder
            mkdir -p /home/builder/build
            cp -r /build/* /home/builder/build/ 2>/dev/null || cp -r /build/. /home/builder/build/ 2>/dev/null || true
            chown -R builder:builder /home/builder/build
            su builder -c 'cd /home/builder/build && makepkg --noconfirm --nodeps --skipinteg'
            cp /home/builder/build/*.pkg.tar.zst /build/ 2>/dev/null || true
        "
}

# Find and copy the package - check both current directory and any subdirectories
PKG=$(find . -name "winetricks-*.pkg.tar.zst" -type f 2>/dev/null | head -1)
if [ -n "$PKG" ] && [ -f "$PKG" ]; then
    echo "Package found: $PKG"
    # Copy to root directory for upload
    cp "$PKG" . 2>/dev/null || true
    # Verify it exists in current directory
    FINAL_PKG=$(basename "$PKG")
    if [ -f "$FINAL_PKG" ]; then
        echo "Package ready for upload: $FINAL_PKG"
        ls -lh "$FINAL_PKG"
    else
        echo "Warning: Could not copy package to root directory"
        ls -lh "$PKG"
    fi
else
    echo "Error: Package file not found"
    echo "Searching for any .pkg.tar files..."
    find . -name "*.pkg.tar.*" -type f 2>/dev/null | head -10
    echo ""
    echo "Listing current directory contents:"
    ls -la
fi

