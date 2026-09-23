# orcr design

This branch holds the design of orcr, and nothing else. orcr is a provider-neutral CLI that
runs and controls AI coding agents of four kinds through one interface: Claude Code, Codex,
Pi and OpenCode.

The code is on `main`. This branch carries the documents only.

## Layout

```
docs/                 the documentation site
  content/docs/       the design documents, one page per file
  app/ lib/ components/   the site itself
```

## Read the documents

Every document is a markdown file under `docs/content/docs/`. Read them there, or run the
site.

```
cd docs
pnpm install
pnpm dev
```

The site needs pnpm 10 or newer. Version 8 rejects the workspace file.

## Deployment

A push to this branch that changes `docs/` builds the site and deploys it to Vercel as a
preview. The address appears in the summary of the workflow run.

The production address stays on the site built from `main`. Promote a preview in Vercel to
change that.
