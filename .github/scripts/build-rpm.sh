#!/bin/bash
set -e

# Get version from Cargo.toml
VERSION=$(grep -m1 '^version' winetricks-cli/Cargo.toml | cut -d'"' -f2)
RELEASE="1"

# Create RPM build directories
mkdir -p rpmbuild/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}

# Create tarball
mkdir -p winetricks-${VERSION}
cp -r winetricks-cli winetricks-gui winetricks-lib winetricks-converter winetricks-${VERSION}/
cp Cargo.toml Cargo.lock README.md COPYING winetricks-${VERSION}/
tar czf rpmbuild/SOURCES/winetricks-${VERSION}.tar.gz winetricks-${VERSION}
rm -rf winetricks-${VERSION}

# Create SPEC file
cat > rpmbuild/SPECS/winetricks.spec << EOF
Name:           winetricks
Version:        ${VERSION}
Release:        ${RELEASE}%{?dist}
Summary:        A fast, modern package manager for Wine
License:        LGPL-2.1-or-later
URL:            https://github.com/Winetricks/winetricks
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  rust >= 1.70
BuildRequires:  cargo
BuildRequires:  openssl-devel
Requires:       wine

%description
Winetricks is a package manager for Wine. This is a fast, modern Rust rewrite
that maintains compatibility with the original winetricks while providing
better performance and a modern GUI.

%prep
%setup -q

%build
export RUSTFLAGS="-C link-arg=-Wl,-z,relro,-z,now"
cargo build --release --bin winetricks
cargo build --release --bin winetricks-gui

%install
mkdir -p %{buildroot}/usr/bin
install -m 755 target/release/winetricks %{buildroot}/usr/bin/
install -m 755 target/release/winetricks-gui %{buildroot}/usr/bin/ || true

mkdir -p %{buildroot}/usr/share/man/man1
mkdir -p %{buildroot}/usr/share/doc/winetricks

%files
/usr/bin/winetricks
/usr/bin/winetricks-gui

%changelog
* $(date '+%a %b %d %Y') Winetricks Contributors <winetricks@example.com> - ${VERSION}-${RELEASE}
- Initial RPM package
EOF

# Build RPM
rpmbuild --define "_topdir $(pwd)/rpmbuild" -ba rpmbuild/SPECS/winetricks.spec

# Copy to target location
mkdir -p target/release/rpmbuild/RPMS
cp -r rpmbuild/RPMS/* target/release/rpmbuild/RPMS/ || true

