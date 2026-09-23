# orchestratr design

this branch holds the design of orchestratr, and nothing else. orchestratr is a provider-neutral
tool that runs and controls AI coding agents of four kinds through one interface:
Claude Code, Codex, Pi and OpenCode. its command line tool is `orcr`.

the code is on `main`. this branch carries the documents only.

## layout

```
docs/                     the documentation site
  content/docs/           the design documents, one page per file
  app/ lib/ components/   the site itself
```

## read the documents

every document is an MDX file under `docs/content/docs/`. read them there, or run the site:

```
cd docs
pnpm install
pnpm dev
```

the site needs pnpm 10 or newer. version 8 rejects the workspace file.

## deployment

a push to this branch that changes `docs/` builds the site and deploys it to Vercel as a
preview. the address appears in the summary of the workflow run.

the production address stays on the site built from `main`. promote a preview in Vercel to
change that.
