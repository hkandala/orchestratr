# Releasing orcr

Releases are tag-driven. Pushing a `vX.Y.Z` tag runs
[`.github/workflows/release.yml`](.github/workflows/release.yml), which builds the
`orcr` binaries, creates a GitHub Release, and (when the publish tokens are
configured) pushes to crates.io and npm.

## Quick release (one command)

```sh
scripts/release.sh patch     # 0.1.0 -> 0.1.1  (bug fixes)
scripts/release.sh minor     # 0.1.0 -> 0.2.0  (backward-compatible features)
scripts/release.sh major     # 0.1.0 -> 1.0.0  (breaking changes)
scripts/release.sh 0.4.2     # or an explicit version
scripts/release.sh minor --dry-run   # preview, change nothing
```

It refuses unless you're on a clean `main` in sync with `origin`, bumps `Cargo.toml`
+ `sdk/ts/package.json` (+ `Cargo.lock`), does a sanity `cargo build`, commits
`chore(release): vX.Y.Z`, tags, and pushes — which triggers the release workflow.
That's the whole flow; the manual steps below are the equivalent, for reference.

## Release flow (manual equivalent)

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

- **One-liner** — `curl -fsSL https://orchestratr.dev/install.sh | sh` (see hosting
  below). Detects the platform, downloads the matching release asset, verifies the
  `.sha256`, and installs `orcr` to `~/.local/bin`. Pin a version:
  `… | sh -s -- v0.1.0`.
- **Prebuilt binary** — download `orcr-<version>-<platform>.tar.gz`
  (`macos-arm64` · `macos-x64` · `linux-x64`) from the
  [GitHub Releases](../../releases) page (each ships a `.sha256`), `tar -xzf` it,
  and put `orcr` on your `PATH`.
- **From crates.io** — `cargo install orchestratr` (builds `orcr` from source).
- **SDK** — `npm i @orchestratr/sdk`.

`orcr` needs a running herdr on your `PATH` at runtime; the SDK and
`orcr scaffold` need Node ≥ 20.

## Hosting the install script (orchestratr.dev)

The installer lives in the repo at [`install.sh`](install.sh); it needs to be served
at a stable URL. The domain is on Cloudflare, so the simplest options:

- **Cloudflare Worker (recommended)** — a Worker on the route `orchestratr.dev/install.sh`
  that returns the script. Full control (correct `content-type: text/plain`, caching,
  optionally pin a version). Fetch it from the repo's raw URL, or paste the script into
  the Worker. Minimal example:

  ```js
  export default {
    async fetch() {
      const r = await fetch(
        "https://raw.githubusercontent.com/hkandala/orchestratr/main/install.sh",
        { headers: { "user-agent": "orchestratr-install" } },
      );
      return new Response(await r.text(), {
        headers: { "content-type": "text/plain; charset=utf-8", "cache-control": "max-age=300" },
      });
    },
  };
  ```

- **Cloudflare Pages** — if you host a site for orchestratr.dev, drop `install.sh` in the
  output directory; it's then served at `/install.sh` as a static file.
- **Redirect Rule** — Rules → Redirect Rules: 302 `orchestratr.dev/install.sh` →
  the raw GitHub URL. Simplest, but only works once the raw file is public.

> **Note.** The repo is public, so the release binaries and raw `install.sh` are
> downloadable without auth and the `curl … | sh` one-liner works for everyone. (If a
> repo is ever made private, the assets need a token — the script honors `GITHUB_TOKEN`.)
