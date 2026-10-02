# Releasing Tilcayo

Releases are published from version tags by `.github/workflows/release.yml`.

## One-time crates.io setup

Configure a crates.io trusted publisher for:

- repository owner: `pgavlin`
- repository: `tilcayo`
- workflow: `release.yml`
- GitHub environment: `crates-io`

Create the `crates-io` environment in the GitHub repository settings. Adding a
required reviewer is recommended so a pushed tag cannot publish without an
explicit approval.

## Release process

1. Update the package version in `Cargo.toml` and refresh `Cargo.lock`.
2. Move relevant entries from `Unreleased` into a versioned section in
   `CHANGELOG.md`.
3. Open and merge the release change after CI passes.
4. Tag the release commit and push only that tag:

   ```console
   git tag -s v0.1.0 -m "tilcayo 0.1.0"
   git push origin v0.1.0
   ```

The release workflow verifies that the tag matches the package version and is
reachable from `origin/main`, reruns tests and packaging, publishes through
crates.io trusted publishing, and creates a GitHub release.

Published crates cannot be overwritten. If a release is defective, yank it and
publish a new patch version rather than moving or reusing its tag.
