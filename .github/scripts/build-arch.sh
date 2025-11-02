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
        set +e
        pacman -Syu --noconfirm rust cargo openssl base-devel || true
        useradd -m -s /bin/bash builder || true
        mkdir -p /home/builder/build
        cp -r /build/* /home/builder/build/ 2>/dev/null || cp -r /build/. /home/builder/build/ 2>/dev/null || true
        chown -R builder:builder /home/builder/build
        
        # Temporarily modify PKGBUILD to remove wine from depends (runtime dependency, not needed for build)
        sed -i 's/^depends=(\x27wine\x27)/depends=()/' /home/builder/build/PKGBUILD || \
        sed -i 's/^depends=("wine")/depends=()/' /home/builder/build/PKGBUILD || \
        sed -i '/^depends=.*wine/s/wine//g' /home/builder/build/PKGBUILD || \
        sed -i 's/depends=(.*wine.*)/depends=()/' /home/builder/build/PKGBUILD || true
        
        # Run makepkg - use --ignorearch and --skipinteg to bypass checks
        su builder -c 'cd /home/builder/build && makepkg --noconfirm --nodeps --skipinteg --ignorearch' || {
            echo 'makepkg failed, trying without skipinteg...'
            su builder -c 'cd /home/builder/build && makepkg --noconfirm --nodeps --ignorearch' || true
        }
        
        # Find and copy any created packages
        find /home/builder/build -name '*.pkg.tar.zst' -type f -exec cp {} /build/ \; 2>/dev/null || true
        echo 'Package search complete'
        ls -lh /build/*.pkg.tar.* 2>/dev/null || echo 'No packages found in /build'
    " || {
    echo "Docker build failed, trying with podman..."
    podman run --rm \
        -v "$(pwd)":/build:ro \
        archlinux:latest \
        bash -c "
            set +e
            pacman -Syu --noconfirm rust cargo openssl base-devel || true
            useradd -m -s /bin/bash builder || true
            mkdir -p /home/builder/build
            cp -r /build/* /home/builder/build/ 2>/dev/null || cp -r /build/. /home/builder/build/ 2>/dev/null || true
            chown -R builder:builder /home/builder/build
            
            # Temporarily modify PKGBUILD to remove wine from depends
            sed -i 's/^depends=(\x27wine\x27)/depends=()/' /home/builder/build/PKGBUILD || \
            sed -i 's/^depends=("wine")/depends=()/' /home/builder/build/PKGBUILD || \
            sed -i '/^depends=.*wine/s/wine//g' /home/builder/build/PKGBUILD || \
            sed -i 's/depends=(.*wine.*)/depends=()/' /home/builder/build/PKGBUILD || true
            
            # Run makepkg
            su builder -c 'cd /home/builder/build && makepkg --noconfirm --nodeps --skipinteg --ignorearch' || {
                echo 'makepkg failed, trying without skipinteg...'
                su builder -c 'cd /home/builder/build && makepkg --noconfirm --nodeps --ignorearch' || true
            }
            
            # Find and copy any created packages
            find /home/builder/build -name '*.pkg.tar.zst' -type f -exec cp {} /build/ \; 2>/dev/null || true
            echo 'Package search complete'
            ls -lh /build/*.pkg.tar.* 2>/dev/null || echo 'No packages found in /build'
        "
}

# Find and copy the package - check both current directory and any subdirectories
# Look for either the original name or any .pkg.tar.zst file
PKG=$(find . -name "*.pkg.tar.zst" -type f 2>/dev/null | head -1)
if [ -n "$PKG" ] && [ -f "$PKG" ]; then
    echo "Package found: $PKG"
    # Copy to root directory for upload
    cp "$PKG" . 2>/dev/null || true
    # Verify it exists in current directory
    FINAL_PKG=$(basename "$PKG")
    if [ -f "$FINAL_PKG" ]; then
        # Rename to Winetricks.rs-<version>
        NEW_NAME="Winetricks.rs-${VERSION}.pkg.tar.zst"
        mv "$FINAL_PKG" "$NEW_NAME" || echo "Failed to rename $FINAL_PKG"
        echo "Package ready for upload: $NEW_NAME"
        ls -lh "$NEW_NAME"
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

