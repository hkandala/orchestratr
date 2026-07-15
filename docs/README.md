# orchestratr docs

the orchestratr documentation site, built with Fumadocs on Next.js. all the docs content lives in `content/docs`.

## run locally

```bash
cd docs
pnpm install
pnpm exec next dev
```

then open http://localhost:3000/docs in your browser.

## build

```bash
pnpm build
```

## where content lives

pages are `content/docs/*.mdx`, and each `meta.json` controls the navigation ordering.

## how it ships

pushes to `main` deploy to Vercel via `.github/workflows/docs-deploy.yml`.
