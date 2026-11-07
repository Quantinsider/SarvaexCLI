# Releasing

1. Bump `version` in `Cargo.toml` (`cargo build` refreshes `Cargo.lock`).
2. Add a section to `CHANGELOG.md`.
3. Commit and push to `main`, wait for CI to pass.
4. `git tag vX.Y.Z && git push origin vX.Y.Z`

The release workflow refuses a tag that does not match the Cargo version, then builds Linux, macOS and Windows archives with checksums and publishes them. `install.sh` and `install.ps1` pick up the latest release automatically.
