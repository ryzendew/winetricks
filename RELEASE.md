# Creating a Release

## Automated Releases (Recommended)

Releases are automatically created when you push a git tag:

```bash
# Create and push a tag (e.g., v0.1.0)
git tag -a v0.1.0 -m "Release version 0.1.0"
git push origin v0.1.0
```

GitHub Actions will automatically:
1. Build binaries for Linux, Windows, and macOS
2. Create release archives (tar.gz for Linux/macOS, zip for Windows)
3. Generate SHA256 checksums
4. Create a GitHub Release with all artifacts attached
5. Generate release notes automatically

## Manual Releases

If you prefer to create releases manually:

1. Go to the GitHub repository
2. Click "Releases" → "Draft a new release"
3. Choose or create a tag (e.g., `v0.1.0`)
4. Fill in release notes
5. Upload the binaries from `target/release/`

## Build Locally

To build release binaries locally:

```bash
# Build all binaries
cargo build --release

# Binaries will be in:
# - target/release/winetricks
# - target/release/winetricks-gui
# - target/release/winetricks-converter
```

## Release Checklist

- [ ] Update version in `Cargo.toml` files
- [ ] Update `CHANGELOG.md` (if exists)
- [ ] Test all binaries locally
- [ ] Create and push git tag
- [ ] Verify GitHub Actions completed successfully
- [ ] Check release artifacts are uploaded correctly

