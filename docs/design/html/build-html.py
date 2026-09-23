#!/usr/bin/env python3
"""Render orcr's markdown design doc into one self-contained HTML page.

Handles the subset of markdown these documents actually use: ATX headings,
paragraphs, fenced code blocks, pipe tables, unordered and ordered lists,
horizontal rules, inline code, and links. No dependencies.

This script lives beside the pages it writes. It reads the markdown from the
parent directory and writes every page into its own directory.
"""

import html
import re
import sys
from pathlib import Path

OUT_DIR = Path(__file__).parent
ROOT = OUT_DIR.parent
DOCS = [
    ("spec", None, "Specification", None),
    ("purpose", "purpose.md", "Purpose",
     "what orcr is and why"),
    ("cli", "cli-reference.md", "CLI and socket API",
     "commands, schema, exit codes"),
    ("impl", "implementation.md", "Implementation",
     "layers and the kind adapter"),
    ("config", "configuration.md", "Configuration",
     "config.toml and what is fixed"),
    ("limits", "limitations.md", "Limitations",
     "what does not work, and why"),

    ("agents", None, "Agents", None),
    ("claude", "agents/claude.md", "Claude Code", "--kind claude"),
    ("codex", "agents/codex.md", "Codex", "--kind codex"),
    ("pi", "agents/pi.md", "Pi", "--kind pi"),
    ("opencode", "agents/opencode.md", "OpenCode", "--kind opencode"),

    ("future", None, "Future work", None),
    ("f-index", "future/README.md", "Overview", "what is left out"),
    ("f-perms", "future/permissions.md", "Permissions", "auto modes, once we can answer"),
    ("f-apisend", "future/api-send.md", "Sending without typing", "two kinds can be handed a message"),
    ("f-dropped", "future/dropped.md", "Dropped items", "cuts and reasons"),
    ("f-headless", "future/headless/overview.md", "Headless", "no terminal"),
    ("f-h-claude", "future/headless/claude.md", "Headless: Claude", None),
    ("f-h-codex", "future/headless/codex.md", "Headless: Codex", None),
    ("f-h-pi", "future/headless/pi.md", "Headless: Pi", None),
    ("f-h-opencode", "future/headless/opencode.md", "Headless: OpenCode", None),
    ("f-tmux", "future/tmux-backend.md", "tmux backend", "instead of herdr"),
    ("f-external", "future/external-agents.md", "External agents",
     "agents orcr did not start"),
]
STATUS_WORDS = {"working", "blocked", "idle", "exited", "unknown"}


def slug(text):
    s = re.sub(r"[^\w\s-]", "", text.lower()).strip()
    return re.sub(r"[\s_]+", "-", s)


def inline(text):
    """Escape, then apply inline markup. Code spans are protected first."""
    spans = []

    def stash(m):
        spans.append(m.group(1))
        return f"\x00{len(spans) - 1}\x00"

    text = re.sub(r"`([^`]+)`", stash, text)
    text = html.escape(text, quote=False)
    text = re.sub(r"\[([^\]]+)\]\((https?://[^)]+)\)",
                  r'<a href="\2" rel="noreferrer">\1</a>', text)
    text = re.sub(r"\*\*(.+?)\*\*", r"<strong>\1</strong>", text)
    # Verified / unverified markers used throughout the docs.
    text = re.sub(r"\((V|U)\)",
                  lambda m: f'<span class="mark {m.group(1).lower()}">{m.group(1)}</span>',
                  text)

    def pop(m):
        return f"<code>{html.escape(spans[int(m.group(1))], quote=False)}</code>"

    return re.sub(r"\x00(\d+)\x00", pop, text)


def outside_tags(pattern, repl, html_text):
    """Apply a substitution only to text that sits outside HTML tags."""
    parts = re.split(r"(<[^>]+>)", html_text)
    for i, part in enumerate(parts):
        if not part.startswith("<"):
            parts[i] = re.sub(pattern, repl, part)
    return "".join(parts)


def cell(text):
    """Table cell: adds glyph, verdict and status colouring on top of inline markup."""
    out = inline(text)
    out = re.sub(r"●", '<span class="g full" aria-label="full">●</span>', out)
    out = re.sub(r"◐", '<span class="g part" aria-label="partial">◐</span>', out)
    out = re.sub(r"○", '<span class="g none" aria-label="none">○</span>', out)
    # Bare V / U verdict tokens, outside code spans and existing badges.
    out = outside_tags(r"\bV\b", '<span class="mark v" title="verified here">V</span>', out)
    out = outside_tags(r"\bU\b", '<span class="mark u" title="unverified">U</span>', out)
    for w in STATUS_WORDS:
        # Not when the word is doing ordinary duty, e.g. "working directory".
        out = outside_tags(rf"\b{w}\b(?! directory| tree| set\b)",
                           rf'<span class="st {w}">{w}</span>', out)
    return out


def render(md):
    lines = md.split("\n")
    out, toc = [], []
    i = 0
    while i < len(lines):
        line = lines[i]

        if line.startswith("```"):
            body, i = [], i + 1
            while i < len(lines) and not lines[i].startswith("```"):
                body.append(lines[i])
                i += 1
            i += 1
            code = html.escape("\n".join(body), quote=False)
            out.append(f"<pre><code>{code}</code></pre>")
            continue

        if re.match(r"^\|.*\|\s*$", line) and i + 1 < len(lines) \
                and re.match(r"^\|[\s:|-]+\|\s*$", lines[i + 1]):
            def row(l):
                return [c.strip() for c in l.strip().strip("|").split("|")]
            head = row(line)
            i += 2
            body = []
            while i < len(lines) and re.match(r"^\|.*\|\s*$", lines[i]):
                body.append(row(lines[i]))
                i += 1
            wide = ' class="wide"' if len(head) > 6 else ""
            th = "".join(f"<th>{cell(c)}</th>" for c in head)
            trs = "".join(
                "<tr>" + "".join(f"<td>{cell(c)}</td>" for c in r) + "</tr>"
                for r in body)
            out.append(f'<div class="tw"><table{wide}><thead><tr>{th}</tr></thead>'
                       f"<tbody>{trs}</tbody></table></div>")
            continue

        m = re.match(r"^(#{1,4})\s+(.*)$", line)
        if m:
            lvl, txt = len(m.group(1)), m.group(2).strip()
            sid = slug(txt)
            if lvl in (2, 3):
                toc.append((lvl, txt, sid))
            out.append(f'<h{lvl} id="{sid}">{inline(txt)}'
                       f'<a class="anchor" href="#{sid}" aria-hidden="true">#</a></h{lvl}>')
            i += 1
            continue

        if re.match(r"^(---+|\*\*\*+)\s*$", line):
            out.append("<hr>")
            i += 1
            continue

        m = re.match(r"^(\s*)([-*]|\d+\.)\s+(.*)$", line)
        if m:
            ordered = bool(re.match(r"\d+\.", m.group(2)))
            items, i = [], i
            while i < len(lines):
                mm = re.match(r"^(\s*)([-*]|\d+\.)\s+(.*)$", lines[i])
                if not mm:
                    # continuation of the previous item
                    if items and lines[i].startswith(("  ", "\t")) and lines[i].strip():
                        items[-1] += " " + lines[i].strip()
                        i += 1
                        continue
                    break
                items.append(mm.group(3).strip())
                i += 1
            tag = "ol" if ordered else "ul"
            lis = "".join(f"<li>{inline(x)}</li>" for x in items)
            out.append(f"<{tag}>{lis}</{tag}>")
            continue

        if not line.strip():
            i += 1
            continue

        para = []
        while i < len(lines) and lines[i].strip() \
                and not re.match(r"^(#{1,4}\s|```|\||\s*[-*]\s|\s*\d+\.\s|---+\s*$)", lines[i]):
            para.append(lines[i].strip())
            i += 1
        if para:
            out.append(f"<p>{inline(' '.join(para))}</p>")

    return "\n".join(out), toc


CSS = """
:root{
  --bg:#fbfbfa; --panel:#fff; --fg:#1b1b1a; --dim:#6b6b66; --line:#e6e5e1;
  --accent:#3b5bdb; --code-bg:#f4f4f1; --full:#2f8a55; --part:#b8860b; --none:#a8a8a2;
  --mark-v:#2f8a55; --mark-u:#b0562a;
}
@media (prefers-color-scheme:dark){
  :root{
    --bg:#16161a; --panel:#1c1c21; --fg:#e6e6e3; --dim:#9a9a94; --line:#2c2c33;
    --accent:#8aa2ff; --code-bg:#22222a; --full:#5fbd84; --part:#d6a63a; --none:#6b6b73;
    --mark-v:#5fbd84; --mark-u:#e08a5a;
  }
}
*{box-sizing:border-box}
html{scroll-behavior:smooth}
body{
  margin:0; background:var(--bg); color:var(--fg);
  font:16px/1.65 ui-sans-serif,-apple-system,"Segoe UI",Inter,Helvetica,Arial,sans-serif;
  -webkit-font-smoothing:antialiased;
}
.wrap{display:grid; grid-template-columns:270px minmax(0,1fr); gap:0; max-width:1400px; margin:0 auto}
nav{
  position:sticky; top:0; height:100vh; overflow-y:auto; padding:28px 20px 60px;
  border-right:1px solid var(--line); font-size:13.5px;
}
nav .brand{font-weight:650; font-size:17px; letter-spacing:-.01em; margin-bottom:2px}
nav .sub{color:var(--dim); font-size:12.5px; margin-bottom:22px}
nav .back{
  display:block; font-size:12.5px; color:var(--dim); text-decoration:none;
  margin:0 0 18px; padding:5px 10px; border:1px solid var(--line); border-radius:6px;
}
nav .back:hover{color:var(--fg); background:var(--code-bg)}
nav .group{
  font-size:11px; color:var(--dim); text-transform:uppercase; letter-spacing:.07em;
  margin:18px 0 6px; font-weight:600;
}
nav .group:first-of-type{margin-top:0}
nav .doc{
  display:block; padding:6px 10px; margin:1px 0; border-radius:6px; color:var(--fg);
  text-decoration:none; border:1px solid transparent;
}
nav .doc .t{display:block; font-weight:550; font-size:13.5px}
nav .doc .b{display:block; font-size:11.5px; opacity:.65; margin-top:1px}
nav .doc:hover{background:var(--code-bg)}
nav .doc.on{background:var(--accent); border-color:var(--accent); color:#fff}
nav .doc.on .f,nav .doc.on .b{opacity:.85}
nav .hint{font-size:11.5px; color:var(--dim); margin:14px 0 4px;
  text-transform:uppercase; letter-spacing:.06em}
nav .toc{margin:6px 0 20px; padding:0 0 0 2px; list-style:none}
nav .toc li{margin:0}
nav .toc a{
  display:block; padding:4px 10px; color:var(--dim); text-decoration:none;
  border-left:2px solid transparent; line-height:1.4;
}
nav .toc a:hover{color:var(--fg)}
nav .toc a.active{color:var(--accent); border-left-color:var(--accent); background:var(--code-bg)}
nav .toc .l3 a{padding-left:24px; font-size:12.5px}
main{padding:44px 52px 140px; min-width:0}
section.doc{display:none}
section.doc.on{display:block}
h1{font-size:30px; letter-spacing:-.02em; margin:0 0 6px}
h2{font-size:21px; letter-spacing:-.015em; margin:56px 0 14px; padding-bottom:8px; border-bottom:1px solid var(--line)}
h3{font-size:16.5px; margin:32px 0 10px; letter-spacing:-.01em}
h4{font-size:15px; margin:22px 0 8px; color:var(--dim)}
p,li{max-width:74ch}
p{margin:0 0 14px}
ul,ol{margin:0 0 16px; padding-left:22px}
li{margin:5px 0}
hr{border:0; border-top:1px solid var(--line); margin:40px 0}
a{color:var(--accent)}
code{
  background:var(--code-bg); padding:1.5px 5px; border-radius:4px; font-size:13px;
  font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;
}
pre{
  background:var(--code-bg); border:1px solid var(--line); border-radius:8px;
  padding:14px 16px; overflow-x:auto; margin:0 0 18px;
}
pre code{background:none; padding:0; font-size:12.5px; line-height:1.55}
.tw{overflow-x:auto; margin:0 0 20px; border:1px solid var(--line); border-radius:8px}
table{border-collapse:collapse; width:100%; font-size:13.5px}
th,td{text-align:left; padding:9px 12px; border-bottom:1px solid var(--line); vertical-align:top}
th{font-weight:600; font-size:12px; text-transform:uppercase; letter-spacing:.04em; color:var(--dim);
   background:var(--code-bg); white-space:nowrap}
tbody tr:last-child td{border-bottom:0}
tbody tr:hover{background:var(--code-bg)}
td code{font-size:12px; white-space:nowrap}
table.wide{font-size:12.5px}
table.wide th,table.wide td{padding:7px 10px}
table.wide th:first-child,table.wide td:first-child{
  position:sticky; left:0; z-index:1; background:var(--bg);
  border-right:1px solid var(--line); font-weight:560;
}
table.wide thead th:first-child{background:var(--code-bg); z-index:2}
table.wide tbody tr:hover td:first-child{background:var(--code-bg)}
h2[id],h3[id]{scroll-margin-top:24px}
.g{font-size:14px; margin-right:3px}
.g.full{color:var(--full)} .g.part{color:var(--part)} .g.none{color:var(--none)}
.mark{
  display:inline-block; min-width:16px; text-align:center; font-size:10.5px; font-weight:700;
  padding:1px 5px; border-radius:4px; vertical-align:2px; letter-spacing:.03em;
}
.mark.v{background:color-mix(in srgb,var(--mark-v) 16%,transparent); color:var(--mark-v)}
.mark.u{background:color-mix(in srgb,var(--mark-u) 16%,transparent); color:var(--mark-u)}
.st{font-weight:560}
.st.working{color:var(--accent)} .st.blocked{color:var(--mark-u)} .st.error{color:#c0392b}
.st.idle,.st.exited,.st.unknown{color:var(--dim)}
.anchor{
  margin-left:8px; color:var(--line); text-decoration:none; font-weight:400;
  opacity:0; transition:opacity .12s;
}
h2:hover .anchor,h3:hover .anchor{opacity:1}
.meta{color:var(--dim); font-size:13.5px; margin-bottom:34px}

/* comments */
mark.cm{
  background:color-mix(in srgb,#f5c542 34%,transparent); color:inherit;
  border-bottom:1.5px solid #d8a318; border-radius:2px; cursor:pointer;
}
mark.cm:hover{background:color-mix(in srgb,#f5c542 55%,transparent)}
#ctip{
  position:absolute; display:none; z-index:60; padding:5px 11px; border:0;
  border-radius:6px; background:var(--accent); color:#fff; font:inherit;
  font-size:12.5px; font-weight:560; cursor:pointer; box-shadow:0 2px 10px rgba(0,0,0,.22);
}
#ctoggle{
  position:fixed; right:18px; bottom:18px; z-index:50; display:flex; align-items:center;
  gap:6px; padding:9px 13px; border:1px solid var(--line); border-radius:22px;
  background:var(--panel); color:var(--fg); cursor:pointer; font:inherit; font-size:13px;
  box-shadow:0 2px 12px rgba(0,0,0,.14);
}
#ctoggle:hover{border-color:var(--accent); color:var(--accent)}
#ccount:empty{display:none}
#ccount{
  min-width:17px; padding:0 5px; border-radius:9px; background:var(--accent);
  color:#fff; font-size:11px; font-weight:700; line-height:17px; text-align:center;
}
#cpanel{
  position:fixed; top:0; right:0; width:360px; height:100vh; z-index:55;
  display:flex; flex-direction:column; background:var(--panel);
  border-left:1px solid var(--line); transform:translateX(100%);
  transition:transform .16s ease; box-shadow:-2px 0 18px rgba(0,0,0,.10);
}
body.cpanel-on #cpanel{transform:none}
body.cpanel-on #ctoggle{display:none}
@media(min-width:1500px){ body.cpanel-on .wrap{margin-right:360px} }
#cpanel header{
  display:flex; align-items:center; gap:7px; padding:13px 14px;
  border-bottom:1px solid var(--line); font-size:14px;
}
#cpanel header .sp{flex:1}
#cpanel header button{
  border:1px solid var(--line); background:none; color:var(--dim); cursor:pointer;
  font:inherit; font-size:11.5px; padding:3px 9px; border-radius:5px;
}
#cpanel header button:hover{color:var(--fg); border-color:var(--fg)}
#cclose{font-size:15px !important; line-height:1; padding:2px 8px !important}
#clist{flex:1; overflow-y:auto; padding:10px 12px 30px}
#clist .empty{color:var(--dim); font-size:13px; padding:14px 2px; max-width:none}
.citem{
  border:1px solid var(--line); border-radius:8px; padding:10px 11px; margin-bottom:10px;
  background:var(--bg);
}
.citem.on{border-color:var(--accent)}
.citem.orphan{opacity:.72}
.csec{font-size:11px; color:var(--dim); text-transform:uppercase; letter-spacing:.04em;
      margin-bottom:6px}
.citem blockquote{
  margin:0 0 7px; padding:5px 9px; border-left:2px solid #d8a318; font-size:12.5px;
  color:var(--dim); background:var(--code-bg); border-radius:0 4px 4px 0;
  max-height:88px; overflow:hidden;
}
.cbody{font-size:13.5px; white-space:pre-wrap; margin-bottom:8px}
.cact{display:flex; gap:6px}
.cact button{
  border:1px solid var(--line); background:none; color:var(--dim); cursor:pointer;
  font:inherit; font-size:11.5px; padding:2px 8px; border-radius:5px;
}
.cact button:hover{color:var(--fg); border-color:var(--fg)}
#cpanel footer{
  padding:9px 14px; border-top:1px solid var(--line); color:var(--dim); font-size:11.5px;
}
@media(max-width:900px){ #cpanel{width:100%} }
#cpop{
  position:fixed; display:none; z-index:70; width:340px; padding:11px;
  background:var(--panel); border:1px solid var(--line); border-radius:9px;
  box-shadow:0 6px 26px rgba(0,0,0,.20);
}
#cpop.on{display:block}
#cpop .q{
  font-size:12px; color:var(--dim); background:var(--code-bg);
  border-left:2px solid #d8a318; border-radius:0 4px 4px 0; padding:5px 8px;
  margin-bottom:8px; max-height:60px; overflow:hidden;
}
#cpop textarea{
  width:100%; resize:vertical; font:inherit; font-size:13.5px; padding:7px 9px;
  border:1px solid var(--line); border-radius:6px; background:var(--bg);
  color:var(--fg);
}
#cpop textarea:focus{outline:none; border-color:var(--accent)}
#cpop .row{display:flex; align-items:center; gap:7px; margin-top:8px}
#cpop .row .sp{flex:1}
#cpop .k{font-size:11px; color:var(--dim)}
#cpop button{
  border:1px solid var(--line); background:none; color:var(--dim); cursor:pointer;
  font:inherit; font-size:12px; padding:4px 11px; border-radius:6px;
}
#cpop button:hover{color:var(--fg); border-color:var(--fg)}
#cpop button.pri{background:var(--accent); border-color:var(--accent); color:#fff}
#cpop button.pri:hover{opacity:.9; color:#fff}
#cclear.armed{color:#c0392b; border-color:#c0392b}
.legend{display:flex; gap:18px; flex-wrap:wrap; font-size:12.5px; color:var(--dim); margin:-6px 0 22px}
@media(max-width:900px){
  .wrap{grid-template-columns:1fr}
  nav{position:static; height:auto; border-right:0; border-bottom:1px solid var(--line)}
  main{padding:28px 20px 80px}
}
"""

JS = """
const docs=[...document.querySelectorAll('section.doc')];
const tabs=[...document.querySelectorAll('nav .doc')];
function show(id,keepScroll){
  docs.forEach(d=>d.classList.toggle('on',d.id==='doc-'+id));
  tabs.forEach(t=>t.classList.toggle('on',t.dataset.doc===id));
  document.querySelectorAll('nav .toc').forEach(u=>u.style.display=u.dataset.doc===id?'':'none');
  if(!keepScroll){history.replaceState(null,'','#'+id); window.scrollTo(0,0);}
}
tabs.forEach(t=>t.addEventListener('click',e=>{e.preventDefault();show(t.dataset.doc)}));
// A section anchor must also switch to the document that contains it.
function docOf(el){const s=el.closest('section.doc'); return s?s.id.slice(4):null;}
document.querySelectorAll('nav .toc a').forEach(a=>a.addEventListener('click',e=>{
  const t=document.getElementById(a.getAttribute('href').slice(1));
  if(t){const d=docOf(t); if(d) show(d,true);}
}));
const links=[...document.querySelectorAll('nav .toc a')];
const spy=new IntersectionObserver(es=>{
  es.forEach(e=>{
    if(!e.isIntersecting) return;
    links.forEach(a=>a.classList.toggle('active',a.getAttribute('href')==='#'+e.target.id));
  });
},{rootMargin:'-80px 0px -75% 0px'});
document.querySelectorAll('h2[id],h3[id]').forEach(h=>spy.observe(h));
const start=location.hash.replace('#','');
const first=docs.length?docs[0].id.slice(4):'';
if(docs.some(d=>d.id==='doc-'+start)){show(start);}
else{
  const t=start&&document.getElementById(start);
  if(t){show(docOf(t)||first,true); t.scrollIntoView();}
  else show(first);
}

/* ---------------- comments ---------------- */

const KEY='orcr-comments-v1';
const main=document.querySelector('main');
const panel=document.getElementById('cpanel');
const listEl=document.getElementById('clist');
const countEl=document.getElementById('ccount');
const tip=document.getElementById('ctip');
let items=[];

const load=()=>{try{items=JSON.parse(localStorage.getItem(KEY))||[]}catch{items=[]}};
const save=()=>localStorage.setItem(KEY,JSON.stringify(items));
const esc=t=>t.replace(/[&<>]/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;'}[c]));

// The section a node sits under, for grouping and re-anchoring.
function sectionOf(node){
  let el=node.nodeType===3?node.parentElement:node;
  const doc=el?(docOf(el)||''):'';
  while(el&&el!==main){
    let p=el.previousElementSibling;
    while(p){ if(/^H[23]$/.test(p.tagName)) return {id:p.id,title:p.textContent.replace(/#$/,'').trim(),doc}; p=p.previousElementSibling; }
    el=el.parentElement;
  }
  return {id:'',title:'(top)',doc};
}

// Nine documents share one page, so a comment must name the one it came from.
function docLabel(key){
  const a=key&&document.querySelector(`nav a.doc[data-doc="${key}"] .t`);
  return a?a.textContent:(key||'');
}
function whereLabel(c){
  const d=docLabel(c.doc), s=c.section||'(top)';
  return d?`${d} · ${s}`:s;
}

// Re-anchor by searching for the quote. Simpler than serialising ranges, and
// it survives a rebuild of the document.
function highlight(c){
  const walk=document.createTreeWalker(main,NodeFilter.SHOW_TEXT);
  let n;
  while((n=walk.nextNode())){
    if(n.parentElement.closest('mark.cm')) continue;
    const i=n.nodeValue.indexOf(c.quote);
    if(i<0) continue;
    const r=document.createRange();
    r.setStart(n,i); r.setEnd(n,i+c.quote.length);
    const m=document.createElement('mark');
    m.className='cm'; m.dataset.id=c.id;
    try{ r.surroundContents(m); }catch{ return false; }
    m.addEventListener('click',()=>focusComment(c.id));
    return true;
  }
  return false;
}

function applyAll(){
  main.querySelectorAll('mark.cm').forEach(m=>{
    const p=m.parentNode; while(m.firstChild) p.insertBefore(m.firstChild,m);
    p.removeChild(m); p.normalize();
  });
  items.forEach(c=>{ c.anchored=highlight(c); });
}

function render(){
  countEl.textContent=items.length||'';
  document.body.classList.toggle('has-comments',items.length>0);
  if(!items.length){ listEl.innerHTML='<p class="empty">Select any text in the document to comment on it.</p>'; return; }
  listEl.innerHTML=items.map(c=>`
    <div class="citem${c.anchored?'':' orphan'}" data-id="${c.id}">
      <div class="csec">${esc(whereLabel(c))}${c.anchored?'':' · text moved'}</div>
      <blockquote>${esc(c.quote)}</blockquote>
      <div class="cbody">${esc(c.text)}</div>
      <div class="cact">
        <button data-act="goto">go to</button>
        <button data-act="edit">edit</button>
        <button data-act="del">delete</button>
      </div>
    </div>`).join('');
}

function focusComment(id){
  openPanel();
  const el=listEl.querySelector(`.citem[data-id="${id}"]`);
  if(!el) return;
  listEl.querySelectorAll('.citem').forEach(x=>x.classList.remove('on'));
  el.classList.add('on');
  el.scrollIntoView({block:'nearest'});
}

listEl.addEventListener('click',e=>{
  const b=e.target.closest('button'); if(!b) return;
  const id=b.closest('.citem').dataset.id;
  const c=items.find(x=>x.id===id); if(!c) return;
  if(b.dataset.act==='del'){ items=items.filter(x=>x.id!==id); save(); applyAll(); render(); }
  if(b.dataset.act==='edit'){
    openPop({quote:c.quote, text:c.text, rect:b.getBoundingClientRect(),
             onSave:t=>{ c.text=t; save(); render(); }});
  }
  if(b.dataset.act==='goto'){
    const c=items.find(x=>x.id===id);
    if(c&&c.doc&&document.getElementById('doc-'+c.doc)) show(c.doc);
    const m=main.querySelector(`mark.cm[data-id="${id}"]`); if(m) m.scrollIntoView({block:'center'});
  }
});

/* one popover, used for both new and edit */
const pop=document.getElementById('cpop');
const popQ=pop.querySelector('.q'), popT=pop.querySelector('textarea');
let onSaveFn=null;

function openPop({quote,text,rect,onSave}){
  onSaveFn=onSave;
  popQ.textContent=quote;
  popT.value=text||'';
  pop.classList.add('on');
  const w=pop.offsetWidth, h=pop.offsetHeight, pad=10;
  let left=Math.min(rect.left, window.innerWidth-w-pad);
  let top=rect.bottom+8;
  if(top+h>window.innerHeight-pad) top=Math.max(pad, rect.top-h-8);
  pop.style.left=Math.max(pad,left)+'px';
  pop.style.top=top+'px';
  popT.focus();
}
function closePop(){ pop.classList.remove('on'); onSaveFn=null; }

pop.addEventListener('click',e=>{
  const b=e.target.closest('button'); if(!b) return;
  if(b.dataset.a==='cancel') return closePop();
  const t=popT.value.trim(); if(!t) return closePop();
  const fn=onSaveFn; closePop(); if(fn) fn(t);
});
popT.addEventListener('keydown',e=>{
  if(e.key==='Escape'){ e.preventDefault(); closePop(); }
  if(e.key==='Enter'&&(e.metaKey||e.ctrlKey)){ e.preventDefault(); pop.querySelector('[data-a=save]').click(); }
});
document.addEventListener('mousedown',e=>{
  if(pop.classList.contains('on')&&!e.target.closest('#cpop')&&!e.target.closest('#ctip')) closePop();
});

/* selection -> floating add button */
let pending=null;
document.addEventListener('mouseup',e=>{
  const t=e.target instanceof Element?e.target:null;
  if(t&&(t.closest('#cpanel')||t.closest('#ctip'))) return;
  setTimeout(()=>{
    const sel=window.getSelection();
    const txt=sel&&sel.toString().trim();
    if(!txt||!sel.rangeCount){ tip.style.display='none'; pending=null; return; }
    const r=sel.getRangeAt(0);
    if(!main.contains(r.commonAncestorContainer)){ tip.style.display='none'; return; }
    pending={quote:txt,...sectionOf(r.startContainer)};
    const b=r.getBoundingClientRect();
    tip.style.display='block';
    tip.style.top=(window.scrollY+b.bottom+8)+'px';
    tip.style.left=(window.scrollX+b.left)+'px';
  },0);
});

tip.addEventListener('click',()=>{
  if(!pending) return;
  const p=pending, r=tip.getBoundingClientRect();
  tip.style.display='none';
  openPop({quote:p.quote, text:'', rect:r, onSave:t=>{
    items.push({id:String(Object.keys(items).length+1)+'-'+p.quote.length+'-'+p.quote.slice(0,8),
                quote:p.quote, section:p.title, sectionId:p.id, doc:p.doc, text:t});
    save(); applyAll(); render();
    const sel=window.getSelection(); if(sel) sel.removeAllRanges();
  }});
  pending=null;
});

/* panel + copy */
function openPanel(){ document.body.classList.add('cpanel-on'); }
document.getElementById('ctoggle').addEventListener('click',()=>document.body.classList.toggle('cpanel-on'));
document.getElementById('cclose').addEventListener('click',()=>document.body.classList.remove('cpanel-on'));

document.getElementById('ccopy').addEventListener('click',async()=>{
  if(!items.length) return;
  const out=items.map((c,i)=>
    `### ${i+1}. ${whereLabel(c)}\\n\\n> ${c.quote.replace(/\\n/g,'\\n> ')}\\n\\n${c.text}`
  ).join('\\n\\n');
  const body=`# orcr design comments (${items.length})\\n\\n${out}\\n`;
  try{ await navigator.clipboard.writeText(body); flash('copied'); }
  catch{ const t=document.createElement('textarea'); t.value=body; document.body.appendChild(t);
         t.select(); document.execCommand('copy'); t.remove(); flash('copied'); }
});

const clearBtn=document.getElementById('cclear');
let armed=false, armTimer=null;
clearBtn.addEventListener('click',()=>{
  if(!items.length) return;
  if(!armed){
    armed=true; clearBtn.textContent='sure?'; clearBtn.classList.add('armed');
    armTimer=setTimeout(()=>{armed=false; clearBtn.textContent='clear';
                             clearBtn.classList.remove('armed');},3000);
    return;
  }
  clearTimeout(armTimer); armed=false;
  clearBtn.textContent='clear'; clearBtn.classList.remove('armed');
  items=[]; save(); applyAll(); render();
});

function flash(msg){
  const b=document.getElementById('ccopy'), old=b.textContent;
  b.textContent=msg; setTimeout(()=>b.textContent=old,1200);
}

load(); applyAll(); render();
"""


def build(doc_list, out_path, page_title, back=False):
    sections, navs = [], []
    if back:
        navs.append('<a class="back" href="index.html">&#8592; all documents</a>')
    for key, fname, label, blurb in doc_list:
        if fname is None:                       # a heading in the sidebar
            navs.append(f'<div class="group">{html.escape(label)}</div>')
            continue
        path = ROOT / fname
        if not path.exists():
            sys.exit(f"missing {path}")
        body, toc = render(path.read_text())
        sections.append(f'<section class="doc" id="doc-{key}">{body}</section>')
        items = "".join(
            f'<li class="l{lvl}"><a href="#{sid}">'
            f'{html.escape(txt.replace("`", ""))}</a></li>'
            for lvl, txt, sid in toc)
        sub = f'<span class="b">{html.escape(blurb)}</span>' if blurb else ""
        navs.append(
            f'<a class="doc" data-doc="{key}" href="#{key}" title="{fname}">'
            f'<span class="t">{html.escape(label)}</span>{sub}</a>'
            f'<ul class="toc" data-doc="{key}">{items}</ul>')

    page = f"""<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{page_title}</title>
<link rel="icon" href="data:image/svg+xml,
  %3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 32 32'%3E
  %3Crect width='32' height='32' rx='7' fill='%233b5bdb'/%3E
  %3Ctext x='16' y='23' font-family='monospace' font-size='19' font-weight='700'
  fill='white' text-anchor='middle'%3Eo%3C/text%3E%3C/svg%3E">
<style>{CSS}</style>
</head><body>
<div class="wrap">
  <nav>
    <div class="brand">orcr</div>
    <div class="sub">one CLI for coding agent sessions</div>
    {''.join(navs)}
  </nav>
  <main>{''.join(sections)}</main>
</div>

<button id="ctoggle" title="Comments">
  <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor"
       stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
    <path d="M21 11.5a8.4 8.4 0 0 1-9 8.4 8.5 8.5 0 0 1-3.8-.9L3 21l1.9-5.1A8.4 8.4 0 0 1 4 11.5a8.4 8.4 0 0 1 9-8.4 8.4 8.4 0 0 1 8 8.4z"/>
  </svg>
  <span id="ccount"></span>
</button>

<aside id="cpanel">
  <header>
    <strong>Comments</strong>
    <div class="sp"></div>
    <button id="ccopy">copy all</button>
    <button id="cclear">clear</button>
    <button id="cclose" title="Close">&#215;</button>
  </header>
  <div id="clist"></div>
  <footer>Stored in this browser only. Use copy all to paste them back.</footer>
</aside>

<button id="ctip">Comment</button>

<div id="cpop">
  <div class="q"></div>
  <textarea rows="4" placeholder="Your comment"></textarea>
  <div class="row">
    <span class="k">&#8984;&#9166; to save &#183; esc to cancel</span>
    <div class="sp"></div>
    <button data-a="cancel">cancel</button>
    <button data-a="save" class="pri">save</button>
  </div>
</div>

<script>{JS}</script>
</body></html>
"""
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(page)
    return len(page)


def main():
    outdir = OUT_DIR
    total = build(DOCS, outdir / "index.html", "orcr — design")
    print(f"wrote {outdir / 'index.html'}  ({total / 1024:.0f} KB)  "
          f"{sum(1 for d in DOCS if d[1])} documents")

    for key, fname, label, blurb in DOCS:
        if fname is None:
            continue
        name = fname.replace("/", "-").removesuffix(".md") + ".html"
        n = build([(key, fname, label, blurb)], outdir / name,
                  f"orcr — {label}", back=True)
        print(f"  {name}  ({n / 1024:.0f} KB)")


if __name__ == "__main__":
    main()
