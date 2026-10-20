# Release runbook (HORO-1935)

How a Glomeris release is cut so that no unsigned or unverified app can become
the public artifact. Written for the person holding the Apple account and the
repository admin role.

## What a tag does and does not do

| Step | Trigger | Effect |
|---|---|---|
| `Release` (`release.yml`, dist-managed) | pushing a tag such as `v0.3.0-rc.1` | Builds the `aarch64` and `x86_64` CLI tarballs, uploads them to a release that **must already exist as a draft**, then publishes it. With no draft the host job fails and nothing becomes public. |
| Homebrew formula | same run | Runs for a non-pre-release tag (`announcement_is_prerelease` false); without `HOMEBREW_TAP_TOKEN` that job **fails** and `announce` is skipped. A pre-release skips it; `glomeris.rb` is still attached as an asset. |
| `macOS App Release` (`macos-app-release.yml`) | manual dispatch (or a human publishing a release) | Signs, notarizes and attaches the app. Refuses to run without every signing secret unless dispatched with `allow_unsigned_dev` against a pre-release, in which case the asset is named `GlomerisMenuBar-DEV-ONLY-unsigned.app.zip`. |

A release created by `GITHUB_TOKEN` does not trigger `macos-app-release.yml`
(GitHub does not start workflows from token-created events). That was measured:
v0.2.0 has no app asset and the app workflow has never run. The app is built by
dispatching the workflow after the release is public.

`create-release = false` means dist undrafts the release itself once its
artifacts are uploaded. The draft is therefore a guard against an accidental
tag push, not a review gate after the build. The review gate is the dry run
below, done before the tag.

## Owner prerequisites (nothing here is available on the development host)

1. Apple Developer Program membership (99 USD per year per Apple's published
   price; an organization also needs a D-U-N-S number). Only the Account
   Holder can create a Developer ID certificate for an individual team.
2. A **Developer ID Application** certificate exported as a `.p12` with a
   password.
3. An App Store Connect **API key** (`.p8`) with its key id and issuer id, for
   `notarytool`.
4. Repository Actions secrets, all required together:
   `APPLE_DEVELOPER_ID_CERT_P12_BASE64`, `APPLE_DEVELOPER_ID_CERT_PASSWORD`,
   `APPLE_SIGNING_IDENTITY`, `APPLE_NOTARY_API_KEY_P8_BASE64`,
   `APPLE_NOTARY_KEY_ID`, `APPLE_NOTARY_ISSUER_ID`.
5. For a stable (non-pre-release) tag: `HOMEBREW_TAP_TOKEN` (HORO-1320).

The signing secrets are plain repository secrets, so any dispatch by someone with
write access can use them. Before the first signed release, move them into a
GitHub Environment (required reviewers, deployment branches limited to `main`)
and add `environment:` to the job. The workflow builds the app from the tagged
commit, not from the dispatched branch.

The dist CLI tarballs are **not** Developer ID signed or notarized and are public
as soon as the draft is undrafted by a tag push; only the app is gated.

Do not paste any of these into an issue, a chat or a log. Set them with
`gh secret set NAME` from a terminal, reading the value from stdin.

## Dry run (before any tag)

All of this is read-only or local; none of it needs a credential.

1. `dist plan --tag vX.Y.Z[-rc.N] --output-format=json` at the exact commit to
   release. Check `announcement_tag`, `announcement_is_prerelease`,
   `publish_prereleases` and the artifact list.
2. Build the candidate from that commit (`cargo build --release --locked`),
   package it like dist does, and run
   `scripts/verify-release-candidate.sh --tarball <tarball> --expect-version X.Y.Z[-rc.N]`.
3. `bash scripts/check-release-version-is-taggable.sh` and the rest of the
   guards CI runs.
4. Confirm the signing secrets exist (names only): `gh secret list`.
5. Check for stale drafts first (`gh release list`, look for `Draft`): a leftover
   draft is published by the next tag push. Delete any you did not just create.
6. Create the draft: `gh release create vX.Y.Z[-rc.N] --draft --prerelease
   --title ... --notes ...` (drop `--prerelease` for a stable tag).

## Cutting the release

1. Push the tag. Watch `Release`. It uploads the tarballs and undrafts.
2. Re-run `scripts/verify-release-candidate.sh` against the **published**
   tarball for each architecture. The `x86_64` tarball needs an Intel host or
   Rosetta to execute; on a host without either, verify it statically
   (`lipo -archs`, checksum) and say so.
3. Dispatch `macOS App Release` with the tag. Expect `signing mode: signed`.
4. Download `GlomerisMenuBar.app.zip` through a browser (so it is quarantined),
   unzip, run `spctl --assess --type execute -vv` and `xcrun stapler validate`,
   and open it by double-click on a clean user account.

## Rollback

* Release run failed before publishing: the draft is still a draft. Delete it
  with `gh release delete vX.Y.Z --cleanup-tag`. Nothing public changed.
* Published, and something is wrong: mark it a pre-release or revert it to
  draft (`gh release edit vX.Y.Z --draft`), delete the bad app asset
  (`gh release delete-asset`), and leave the tag in place until a fixed tag
  supersedes it. Do not move or reuse a published tag. A release reverted to
  draft is re-armed: re-pushing that tag or re-running `Release` republishes it,
  so delete the draft (and tag) once the fixed tag exists.
* Homebrew formula was published: revert the formula commit in
  `Chisanan232/homebrew-tap` to the previous version. A pre-release never
  touches the tap.
* A signing credential is suspected exposed: revoke the Developer ID
  certificate and the API key in the Apple account, then rotate the secrets.
