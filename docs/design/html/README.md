# HTML build

This folder holds the design documents rendered for reading and commenting, and the script
that makes them. Everything here is generated except `build-html.py` and this file.

## Build

```
cd docs/design/html
python3 build-html.py
```

The script needs Python 3 and nothing else. It reads the markdown from the parent directory
and writes every page into this one. It is safe to run again at any time, and it overwrites
each page.

Run it after every change to a markdown file. Nothing checks that the pages are current.

## Output

| File | Contents |
|---|---|
| `index.html` | Every document in one page, with a sidebar to move between them |
| `<name>.html` | One document, with a link back to `index.html` |

A single document file is named for its source. A file in a subdirectory keeps the path, with
each slash replaced by a hyphen: `agents/codex.md` becomes `agents-codex.html`.

Each page carries its own style and script, so a page works from a file path and needs no
server and no network.

## Adding a document

A new markdown file does not appear until you add it to the `DOCS` list near the top of
`build-html.py`. Each entry holds four values.

| Value | Purpose |
|---|---|
| key | Short name used in the address, as `#key` |
| path | The markdown file, relative to the parent directory |
| label | The name in the sidebar |
| blurb | One line under the label, or `None` |

An entry whose path is `None` becomes a heading in the sidebar. The script stops with an
error when a listed file is absent.

## Comments

Each page carries a comment tool. Select any text, write a note, and the note stays in that
browser. Nothing is sent anywhere, and nothing is written back to the markdown.

Use "copy all" in the comment panel to get every note as markdown, then paste it where it is
needed. Comments live in browser storage, so they are lost when that storage is cleared, and
a person on another machine does not see them.

A comment finds its place again by searching for the text it quotes. When the quoted text
changes, the comment stays in the panel and is marked as moved.
