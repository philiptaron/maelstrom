# Releasing

These are the items we need to do to release:
  - Set environment variables.
  - Update `CHANGELOG.md`.
  - Update `version` in `[workspace.package]` in `Cargo.toml`.
  - Commit above changes.
  - Tag previous changeset with the version tag.
  - Push changesets and tags.
  - Publish to crates.io.
  - Update `CHANGELOG.md` for unreleased changes.
  - Update `version` in `[workspace.package]` in `Cargo.toml` to next dev version.
  - Commit above changes.
  - Push changesets.
  - Unset environment variables.
  - Create GitHub release.
  - Update GitHub milestones.

## Set Environment Variables

If you want to be able to cut and past below, set `VERSION` and `NEXT_VERSION`:
```bash
VERSION=1.2.0
NEXT_VERSION=1.3.0
```

## Update `CHANGELOG.md`

To update `CHANGELOG.md`, we need to first add missing items, and then change
the headings to reflect the new release.

### Add Missing Items

The first thing to do when updating the `CHANGELOG.md` is to ensure it is complete.
To do this, look at the `[Unreleased]` link at the bottom of the file in your
browser and fill in the missing items in the `[Unreleaed]` section. If there is
no `[Unreleased]` section yet, create one, then fill it in. Cross-reference
these changes to the closed issues in GitHub for the release. Make sure you add
links to issues when appropriate.

Generally, only add things to `CHANGELOG.md` that you think users would care
about. There need not be an entry for every changeset or even issue.

You should set commit these changes in a separate changeset.

```bash
$EDITOR CHANGELOG.md
git commit -am "Update CHANGELOG.md for version $VERSION"
```

### Change Headings

The second thing to do is to update the headers in `CHANGELOG.md`. This is
mechanical. In the future, we will do this using a tool.

These are the steps to take:
  - At the top of the file, change the `[Unreleased]` header to contain the
    version number and the date. Something like this:
```
## [10.3.1] - 2028-06-26
```
  - Go to the bottom of the file and change the `[unreleased]` line. Change
    "`unreleased`" to the new version number and change "`HEAD`" to the new
    release's tag.

## Update `version` in `[workspace.package]` in `Cargo.toml`

This is a bit tricky because you have to update the version itself as well as
all internal dependencies:
```bash
cargo set-version $VERSION
```

## Commit Above Changes

This commit should only include the mechanical changes to `CHANGELOG.md`,
`Cargo.toml`, and `Cargo.lock`.
```bash
git commit -am "Version $VERSION"
```

## Tag Previous Changeset with the Version Tag

Tag the last committed changeset with the release's tag.
```bash
git tag v$VERSION
```

## Push Changesets and Tags

Ensure that you push, and that you push the tags along with the changesets. We
can do that atomically like this:
```bash
git push --atomic origin main v$VERSION
```

## Publish to Crates.io

Packages are published to crates.io individually. Moreover, they have to be
published in the right order, such that all a package's depdendencies are
published before the package itself. We'll automate this at some point, but for
now:

```bash
for i in crates/{maelstrom-{base,plot,simex,test,worker-child,util,web,worker,broker,container,client,client-cli},cargo-maelstrom}; do (cd $i && cargo publish); done
```

To do this, you must have a secret stored in `~/.cargo/credentials.toml`, and
that secret must allow you to publish these crates. If that is not the case,
ask Neal for the secret and then run `cargo login`.

## Update `CHANGELOG.md` for Unreleased Changes

Add the `[Unreleased]` section into `CHANGELOG.md`. This should look like:
```
## [Unreleased]
### General
#### Changed
#### Added
#### Removed
#### Fixed
### `cargo-maelstrom`
#### Changed
#### Added
#### Removed
#### Fixed
```

At the bottom of the file, add a link like this:
```
[unreleased]: https://github.com/maelstrom-software/maelstrom/compare/v<VERSION>...HEAD
```

## Update `version` in `[workspace.package]` in `Cargo.toml` to Next Dev Version

Use `cargo set-version` again:
```bash
cargo set-version $NEXT_VERSION-dev
```

## Commit Above Changes

It's important to commit these new changes right away, so that nothing other that the
actual version has the given version string.

```bash
git commit -am "Update CHANGELOG.md for version $NEXT_VERSION"
```

## Push Changesets and Tags

Ensure that you push again:
```bash
git push
```

## Unset Environment Variables
```bash
unset VERSION NEXT_VERSION
```

## Create GitHub Release

Go to the [GitHub releases page](https://github.com/maelstrom-software/maelstrom/releases). Click `Draft a
new release`. Use the tag you just crated to draft the release.

## Update GitHub Milestones

Go to the [GitHub milestones page](https://github.com/maelstrom-software/maelstrom/milestones).

If there isn't yet a milestone for the next release, create one.

Then, look at all of the issues in the milestone for the just-released version.
If there are any open issues, either close them or move them out of the
release, as appropriate.

Finally, close the milestone for the just-released version.

## Announce the Release on Discord

Go to the [`#general` channel on the community discord
server](https://discord.gg/nyaGuzJr). Announce the release.