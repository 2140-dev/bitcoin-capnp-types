# Release and Versioning Policy

This document defines the release rules for `bitcoin-capnp-types` so branch history, tags, GitHub releases, and `crates.io` versions stay aligned.

## Scope

This policy applies to:

- branch release flow
- git tags
- GitHub releases
- crates.io publishes

## Branch Model

- `master` tracks the current active development line, i.e. Bitcoin Core `master`.
- Maintenance branches (for example `30.x`, `31.x`) track stable backport lines, i.e. the Bitcoin Core branch of the same name.
- A release must be cut from the branch that owns that version line.

### Maintenance Branches

- A maintenance branch is created from `master` after Bitcoin Core branches off, at a point where the schema files match the new Bitcoin Core branch.
- Its CI builds the Bitcoin Core branch of the same name, and runs for pushes and pull requests that target the maintenance branch.
- After creation it only receives backports from `master` and fixes specific to that branch. It must never be fast-forwarded to, or merged with, `master`.
- The schema files must keep matching the corresponding Bitcoin Core branch. Interface changes are only backported if Bitcoin Core backported them. Test and build changes can be backported more freely.
- Backported commits reference the original pull request and commit on `master`, using the Bitcoin Core convention:

  ```
  Github-Pull: #<number>
  Rebased-From: <commit hash on master>
  ```

## Version Line Mapping

- `30.x` branch -> `0.1.x`
- `31.x` branch -> `0.2.x`
- `32.x` branch -> `0.3.x`
- Future Bitcoin Core branch lines should get a new crate minor line (`0.N.x`).

## Required Invariants

For every released version `X.Y.Z`, all of the following must exist and match:

1. `Cargo.toml` version is `X.Y.Z` on the release commit.
2. Git tag `vX.Y.Z` points to that exact commit.
3. Tag `vX.Y.Z` is reachable from the intended release branch.
4. A GitHub release exists for `vX.Y.Z`.
5. A crates.io release exists for `X.Y.Z`.

No detached tags and no crates publish without corresponding tag and GitHub release.

## Release Checklist

1. Choose release branch (`master` or maintenance branch).
2. Update version in `Cargo.toml`.
3. Run tests and merge the release PR.
4. Create and push annotated tag `vX.Y.Z` on the merged commit.
5. Publish crate `X.Y.Z` to crates.io.
6. Create GitHub release for `vX.Y.Z`.
7. Verify branch/tag/release parity.

## Verification Commands

```sh
git tag --sort=version:refname
git branch --contains vX.Y.Z
gh release view vX.Y.Z
cargo metadata --no-deps
```

Use `Cargo.toml` and crates.io UI/API to verify the published crate version if needed.

## Backfill and Repair Procedure

If a historical mismatch is found (missing tag, missing GitHub release, or tag not on a branch), repair immediately by:

1. creating or fixing the correct tag placement,
2. publishing missing GitHub release metadata,
3. documenting what was corrected in the PR description.

## Enforcement

Any PR that bumps crate version or prepares a release should include a checklist confirming:

- branch line is correct,
- tag plan is defined,
- GitHub release will be created,
- crates.io publish is accounted for.
