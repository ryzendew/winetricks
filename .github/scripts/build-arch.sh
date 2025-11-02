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
# Note: We mount the workspace which includes the built binaries
docker run --rm \
    -v "$(pwd)":/build:ro \
    -e PKGEXT=".pkg.tar.zst" \
    archlinux:latest \
    bash -c "
        set +e
        pacman -Syu --noconfirm rust cargo openssl base-devel || true
        useradd -m -s /bin/bash builder || true
        mkdir -p /home/builder/build
        # Copy repo files
        cp -r /build/* /home/builder/build/ 2>/dev/null || cp -r /build/. /home/builder/build/ 2>/dev/null || true
        # Explicitly copy target directory if it exists (binaries from previous build step)
        if [ -d /build/target ]; then
            cp -r /build/target /home/builder/build/ 2>/dev/null || true
        fi
        chown -R builder:builder /home/builder/build
        # Verify binaries exist
        echo 'Checking for binaries...'
        ls -la /home/builder/build/target/release/ 2>/dev/null || echo 'No target/release directory found'
        if [ ! -f /home/builder/build/target/release/winetricks ]; then
            echo 'ERROR: Binary winetricks not found in target/release/'
            echo 'Building binaries inside Docker...'
            su builder -c 'cd /home/builder/build && cargo build --release --bin winetricks --bin winetricks-gui' || echo 'Build failed'
        fi
        ls -la /home/builder/build/target/release/winetricks* 2>/dev/null || echo 'Binaries still not found'
        
        # Temporarily modify PKGBUILD to remove wine from depends (runtime dependency, not needed for build)
        # Also ensure source is empty since we're using pre-built binaries
        sed -i 's/^depends=(\x27wine\x27)/depends=()/' /home/builder/build/PKGBUILD || \
        sed -i 's/^depends=("wine")/depends=()/' /home/builder/build/PKGBUILD || \
        sed -i '/^depends=.*wine/s/wine//g' /home/builder/build/PKGBUILD || \
        sed -i 's/depends=(.*wine.*)/depends=()/' /home/builder/build/PKGBUILD || true
        # Remove source requirement if present
        sed -i 's/^source=.*$/source=()/' /home/builder/build/PKGBUILD || true
        sed -i 's/^sha256sums=.*$/sha256sums=()/' /home/builder/build/PKGBUILD || true
        
        # Run makepkg - use --ignorearch and --skipinteg to bypass checks
        su builder -c 'cd /home/builder/build && makepkg --noconfirm --nodeps --skipinteg --ignorearch' || {
            echo 'makepkg failed, trying without skipinteg...'
            su builder -c 'cd /home/builder/build && makepkg --noconfirm --nodeps --ignorearch' || true
        }
        
        # Find and copy any created packages - search thoroughly
        echo 'Searching for package files created by makepkg...'
        find /home/builder/build -type f -name '*.pkg.tar.zst' 2>/dev/null | while read -r pkg; do
            echo "Found package: $pkg"
            cp "$pkg" /build/ 2>/dev/null && echo "Copied to /build: $(basename "$pkg")" || echo "Failed to copy: $pkg"
        done
        # Also search for any other package extensions
        find /home/builder/build -type f \( -name '*.pkg.tar.*' -o -name '*winetricks*.tar*' \) 2>/dev/null | head -5
        echo 'Package search complete'
        echo 'Packages in /build:'
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
            # Copy repo files
            cp -r /build/* /home/builder/build/ 2>/dev/null || cp -r /build/. /home/builder/build/ 2>/dev/null || true
            # Explicitly copy target directory if it exists (binaries from previous build step)
            if [ -d /build/target ]; then
                cp -r /build/target /home/builder/build/ 2>/dev/null || true
            fi
            chown -R builder:builder /home/builder/build
            # Verify binaries exist
            echo 'Checking for binaries...'
            ls -la /home/builder/build/target/release/ 2>/dev/null || echo 'No target/release directory found'
            if [ ! -f /home/builder/build/target/release/winetricks ]; then
                echo 'ERROR: Binary winetricks not found in target/release/'
                echo 'Building binaries inside Docker...'
                su builder -c 'cd /home/builder/build && cargo build --release --bin winetricks --bin winetricks-gui' || echo 'Build failed'
            fi
            ls -la /home/builder/build/target/release/winetricks* 2>/dev/null || echo 'Binaries still not found'
            
            # Temporarily modify PKGBUILD to remove wine from depends
            sed -i 's/^depends=(\x27wine\x27)/depends=()/' /home/builder/build/PKGBUILD || \
            sed -i 's/^depends=("wine")/depends=()/' /home/builder/build/PKGBUILD || \
            sed -i '/^depends=.*wine/s/wine//g' /home/builder/build/PKGBUILD || \
            sed -i 's/depends=(.*wine.*)/depends=()/' /home/builder/build/PKGBUILD || true
            # Remove source requirement if present
            sed -i 's/^source=.*$/source=()/' /home/builder/build/PKGBUILD || true
            sed -i 's/^sha256sums=.*$/sha256sums=()/' /home/builder/build/PKGBUILD || true
            
            # Run makepkg
            su builder -c 'cd /home/builder/build && makepkg --noconfirm --nodeps --skipinteg --ignorearch' || {
                echo 'makepkg failed, trying without skipinteg...'
                su builder -c 'cd /home/builder/build && makepkg --noconfirm --nodeps --ignorearch' || true
            }
            
            # Find and copy any created packages - search thoroughly
            echo 'Searching for package files created by makepkg...'
            find /home/builder/build -type f -name '*.pkg.tar.zst' 2>/dev/null | while read -r pkg; do
                echo "Found package: $pkg"
                cp "$pkg" /build/ 2>/dev/null && echo "Copied to /build: $(basename "$pkg")" || echo "Failed to copy: $pkg"
            done
            # Also search for any other package extensions
            find /home/builder/build -type f \( -name '*.pkg.tar.*' -o -name '*winetricks*.tar*' \) 2>/dev/null | head -5
            echo 'Package search complete'
            echo 'Packages in /build:'
            ls -lh /build/*.pkg.tar.* 2>/dev/null || echo 'No packages found in /build'
        "
}

# Find and copy the package - search thoroughly instead of guessing
echo "Searching for Arch package files..."
echo "Looking in current directory and all subdirectories..."

# Search for any .pkg.tar.zst files recursively
PKG=$(find . -type f -name "*.pkg.tar.zst" 2>/dev/null | grep -v ".git" | head -1)

# If not found, search for any .pkg.tar.* files
if [ -z "$PKG" ] || [ ! -f "$PKG" ]; then
    echo "No .pkg.tar.zst found, searching for any .pkg.tar.* files..."
    PKG=$(find . -type f -name "*.pkg.tar.*" 2>/dev/null | grep -v ".git" | head -1)
fi

# Also check if Docker copied it to a specific location
if [ -z "$PKG" ] || [ ! -f "$PKG" ]; then
    echo "Checking common Docker output locations..."
    for dir in . ./target ./target/release ./release build; do
        if [ -f "$dir"/*.pkg.tar.zst ] 2>/dev/null; then
            PKG=$(ls "$dir"/*.pkg.tar.zst 2>/dev/null | head -1)
            break
        fi
    done
fi

if [ -n "$PKG" ] && [ -f "$PKG" ]; then
    echo "✅ Package found: $PKG"
    echo "   Full path: $(realpath "$PKG" 2>/dev/null || echo "$(pwd)/$PKG")"
    echo "   Size: $(ls -lh "$PKG" | awk '{print $5}')"
    
    # Copy to root directory for upload (if not already there)
    PKG_DIR=$(dirname "$PKG")
    if [ "$PKG_DIR" != "." ]; then
        echo "   Copying from $PKG_DIR to root directory..."
        cp "$PKG" . 2>/dev/null || true
        FINAL_PKG=$(basename "$PKG")
    else
        FINAL_PKG="$PKG"
    fi
    
    # Verify it exists in current directory
    if [ -f "$FINAL_PKG" ]; then
        # Rename to Winetricks.rs-<version>
        NEW_NAME="Winetricks.rs-${VERSION}.pkg.tar.zst"
        if [ "$FINAL_PKG" != "$NEW_NAME" ]; then
            mv "$FINAL_PKG" "$NEW_NAME" || echo "Failed to rename $FINAL_PKG"
        fi
        echo "✅ Package ready for upload: $NEW_NAME"
        ls -lh "$NEW_NAME"
    else
        echo "Warning: Could not copy package to root directory"
        ls -lh "$PKG"
    fi
else
    echo "❌ Error: Package file not found"
    echo ""
    echo "Searching for any .pkg.tar files recursively..."
    find . -type f -name "*.pkg.tar.*" 2>/dev/null | grep -v ".git" | head -10
    echo ""
    echo "Listing current directory tree (limited to 3 levels)..."
    find . -maxdepth 3 -type d 2>/dev/null | head -20
    echo ""
    echo "Checking if makepkg created files with different extensions..."
    find . -type f \( -name "*.pkg.*" -o -name "*winetricks*.tar*" -o -name "*winetricks*.zst" \) 2>/dev/null | grep -v ".git" | head -10
fi

