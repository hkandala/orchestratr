# Releasing orcr

Releases are tag-driven. Pushing a `vX.Y.Z` tag runs
[`.github/workflows/release.yml`](.github/workflows/release.yml), which builds the
`orcr` binaries, creates a GitHub Release, and (when the publish tokens are
configured) pushes to crates.io and npm.

## Release flow

1. **Bump versions** to `X.Y.Z` in both manifests — they must stay in lockstep:
   - `Cargo.toml` → `version = "X.Y.Z"` (the release workflow's version guard
     fails the run if the tag doesn't match this).
   - `sdk/ts/package.json` → `"version": "X.Y.Z"`.

   Then refresh the lockfiles so they aren't dirty:

   ```sh
   cargo update -p orchestratr      # or `cargo build` to touch Cargo.lock
   (cd sdk/ts && npm install)       # refreshes package-lock.json
   ```

2. **Commit** the version bump:

   ```sh
   git add Cargo.toml Cargo.lock sdk/ts/package.json sdk/ts/package-lock.json
   git commit -m "release: vX.Y.Z"
   git push
   ```

3. **Tag and push** — this is what triggers the release:

   ```sh
   git tag vX.Y.Z
   git push --tags
   ```

Watch the run under the repo's Actions tab. The `binaries` job always runs; the
`crates` and `npm` publish jobs run only if their secrets are present (see below)
and never block or fail the release when absent.

## Publishing secrets

Publishing to the package registries is **opt-in**. Until you add these repo
secrets (Settings → Secrets and variables → Actions), those jobs auto-skip and
the release still ships binaries.

| Secret                  | Enables            | Get it from                                   |
| ----------------------- | ------------------ | --------------------------------------------- |
| `CARGO_REGISTRY_TOKEN`  | crates.io publish  | https://crates.io/settings/tokens            |
| `NPM_TOKEN`             | npm publish        | https://www.npmjs.com/settings/~/tokens (Automation token) |

## Name / scope caveats (first release)

- **crates.io** — the crate is `orchestratr`. Crate names are global and
  first-come; confirm `orchestratr` is available (or owned by you) before the
  first publish. `cargo publish` fails loudly on a name clash.
- **npm** — the package is `@orchestratr/sdk`, published with
  `--access public`. The `@orchestratr` scope must exist and be owned by the
  publishing account (create the org/scope on npm first); scoped packages are
  private by default, hence `--access public`.
- Versions are immutable on both registries — you cannot re-publish `X.Y.Z`.
  Bump the patch and re-tag if a publish half-fails.

## How users install

- **Prebuilt binary** — download the `orcr-<version>-<target>.tar.gz` for your
  platform from the [GitHub Releases](../../releases) page (each asset ships a
  `.sha256`), `tar -xzf` it, and put `orcr` on your `PATH`.
- **From crates.io** — `cargo install orchestratr` (builds `orcr` from source).
- **SDK** — `npm i @orchestratr/sdk`.

`orcr` needs a running herdr on your `PATH` at runtime; the SDK and
`orcr scaffold` need Node ≥ 20.
