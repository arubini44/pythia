# Releasing Pythia

## Cutting a release

1. Update `version` in the root `Cargo.toml` (`[workspace.package]`) and run
   `cargo check` so `Cargo.lock` follows.
2. Move the `[Unreleased]` entries in `CHANGELOG.md` under a new version
   heading with today's date and update the compare links at the bottom.
3. Update `<releases>` in `assets/linux/com.ortic.pythia.metainfo.xml`.
4. Commit, then tag and push:

   ```sh
   git tag v0.2.0
   git push origin master v0.2.0
   ```

5. `.github/workflows/release.yml` builds on Linux (x86_64), Windows (x86_64)
   and macOS (Apple Silicon and Intel), and creates a **draft** GitHub
   release with all packages, plain archives and `SHA256SUMS.txt`. Review the
   notes and publish it.

The workflow can also be started by hand (*Actions → Release → Run
workflow*); without a tag it only uploads the packages as workflow artifacts.

## Packages

| Platform | Formats | Notes |
|---|---|---|
| Linux | `.deb`, `.AppImage`, `.tar.gz` | `.deb` depends on `libaio1t64 \| libaio1` for the Instant Client |
| Windows | `.msi` (WiX), `-setup.exe` (NSIS), `.zip` | per-machine install |
| macOS | `.dmg` (containing `Pythia.app`), `.tar.gz` | minimum macOS 11 |

Build locally with `cargo packager --release -p pythia-app [-f deb]`; output
goes to `target/packages`.

The Oracle Instant Client is **not** bundled: its license doesn't allow
redistribution in third-party installers. The README explains how to
install it, and Pythia shows the download link when the client is missing.

## Signing and notarization

Unsigned builds work but trigger warnings (SmartScreen on Windows,
Gatekeeper on macOS). To sign, add these repository secrets; the workflow
passes them to cargo-packager:

**macOS** (Developer ID Application certificate)

| Secret | Content |
|---|---|
| `APPLE_CERTIFICATE` | base64 of the exported `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | its password |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Remo Laubacher (TEAMID)` |
| `APPLE_ID`, `APPLE_PASSWORD` | Apple ID and an app-specific password for notarization |
| `APPLE_TEAM_ID` | the team id |

**Windows** (Authenticode): import the certificate into the runner's store
in a step before packaging and set `certificateThumbprint`, `digestAlgorithm`
(`sha256`) and `timestampUrl` under `[package.metadata.packager.windows]` in
`crates/pythia-app/Cargo.toml`, or use `signCommand` with a cloud signing
service (e.g. Azure Trusted Signing).

**Linux**: packages are unsigned; `SHA256SUMS.txt` lets users verify
downloads. For an apt repository, sign the repository metadata separately.
