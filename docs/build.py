#!/usr/bin/env python3
"""AionDB docs engine.

Walks `content/`, converts each Markdown file to HTML using a self-contained
subset parser, injects every page into the layout template, and emits a static
site under `_site/`. Sidebar navigation is generated from the content tree.

Usage:
    python3 build.py                # build to ./_site
    python3 build.py --check-links  # build and validate local links
    python3 build.py --serve        # build, then serve on localhost:8000
    python3 build.py --out PATH     # build to PATH
"""

from __future__ import annotations

import argparse
import html
import http.server
import os
import re
import shutil
import socketserver
import sys
from pathlib import Path
from typing import Iterable
from urllib.parse import unquote, urlparse

ROOT = Path(__file__).resolve().parent
CONTENT = ROOT / "content"
THEME = ROOT / "theme"
DEFAULT_OUT = ROOT / "_site"
THEME_STYLE_MARKER = "AIONDB_STUDIO_THEME_LOCK:v2"


# ---------------------------------------------------------------------------
# Markdown subset parser
# ---------------------------------------------------------------------------
#
# Supported:
#   - front-matter (--- ... ---) with `title:` and `order:` keys
#   - ATX headings (# .. ######)
#   - paragraphs
#   - bold (**x** / __x__), italic (*x* / _x_), inline code (`x`)
#   - links [text](url)
#   - fenced code blocks (```lang\n...\n```)
#   - unordered lists (-, *)
#   - ordered lists (1. 2. ...)
#   - blockquotes (> ...)
#   - horizontal rules (---)
#
# Intentionally rejects HTML pass-through to keep generated output predictable.


_INLINE_CODE = re.compile(r"`([^`]+)`")
_BOLD = re.compile(r"\*\*([^*]+)\*\*|__([^_]+)__")
_ITAL = re.compile(r"(?<!\*)\*([^*\s][^*]*?)\*(?!\*)|(?<!_)_([^_\s][^_]*?)_(?!_)")
_LINK = re.compile(r"\[([^\]]+)\]\(([^)\s]+)\)")
_FRONT = re.compile(r"^---\s*\n(.*?)\n---\s*\n", re.DOTALL)


def render_inline(text: str) -> str:
    placeholders: list[str] = []

    def stash(rendered: str) -> str:
        placeholders.append(rendered)
        return f"\x00{len(placeholders) - 1}\x00"

    out = _INLINE_CODE.sub(
        lambda m: stash(f"<code>{html.escape(m.group(1), quote=False)}</code>"),
        text,
    )
    out = _LINK.sub(
        lambda m: stash(
            f'<a href="{html.escape(m.group(2), quote=True)}">'
            f"{html.escape(m.group(1), quote=False)}</a>"
        ),
        out,
    )
    out = html.escape(out, quote=False)
    out = _BOLD.sub(lambda m: f"<strong>{m.group(1) or m.group(2)}</strong>", out)
    out = _ITAL.sub(lambda m: f"<em>{m.group(1) or m.group(2)}</em>", out)

    for i, value in enumerate(placeholders):
        out = out.replace(f"\x00{i}\x00", value)
    return out


def parse_front_matter(text: str) -> tuple[dict[str, str], str]:
    match = _FRONT.match(text)
    if not match:
        return {}, text
    meta: dict[str, str] = {}
    for line in match.group(1).splitlines():
        if ":" not in line:
            continue
        key, _, value = line.partition(":")
        meta[key.strip().lower()] = value.strip().strip('"').strip("'")
    return meta, text[match.end():]


_BULLET_PREFIX = re.compile(r"^[-*]\s+")
_ORDERED_PREFIX = re.compile(r"^\d+\.\s+")
_BLOCK_BREAK = (
    re.compile(r"^#{1,6}\s+"),    # heading
    re.compile(r"^>"),             # blockquote
    re.compile(r"^```"),           # fenced code
    re.compile(r"^-{3,}\s*$"),     # horizontal rule
    re.compile(r"^\|"),            # table row
)


def _is_block_break(stripped: str) -> bool:
    return any(p.match(stripped) for p in _BLOCK_BREAK)


def collect_list_items(
    lines: list[str], start: int, n: int, ordered: bool
) -> tuple[int, list[str]]:
    """Collect contiguous list items, folding wrapped continuation lines.

    A continuation line is a non-empty line that:
    - is not a new bullet of the same list kind,
    - is not a new bullet of the other list kind,
    - is not the start of another block construct (heading, fence, ...).

    Continuation lines are joined to the previous item with a single space, so
    paragraphs wrapped over multiple source lines render inside one `<li>`.
    """
    items: list[str] = []
    i = start
    prefix = _ORDERED_PREFIX if ordered else _BULLET_PREFIX
    other = _BULLET_PREFIX if ordered else _ORDERED_PREFIX
    while i < n:
        stripped = lines[i].strip()
        if not stripped:
            break
        if prefix.match(stripped):
            items.append(prefix.sub("", stripped))
            i += 1
            continue
        if other.match(stripped) or _is_block_break(stripped):
            break
        if not items:
            break
        items[-1] = f"{items[-1]} {stripped}"
        i += 1
    return i, items


def render_markdown(source: str) -> str:
    lines = source.splitlines()
    out: list[str] = []
    i = 0
    n = len(lines)

    def flush_paragraph(buf: list[str]) -> None:
        if not buf:
            return
        joined = " ".join(line.strip() for line in buf).strip()
        if joined:
            out.append(f"<p>{render_inline(joined)}</p>")
        buf.clear()

    para: list[str] = []

    while i < n:
        line = lines[i]
        stripped = line.strip()

        if not stripped:
            flush_paragraph(para)
            i += 1
            continue

        if stripped.startswith("```"):
            flush_paragraph(para)
            lang = stripped[3:].strip()
            i += 1
            buf: list[str] = []
            while i < n and not lines[i].strip().startswith("```"):
                buf.append(lines[i])
                i += 1
            i += 1  # skip closing fence
            cls = f' class="lang-{html.escape(lang, quote=True)}"' if lang else ""
            out.append(f"<pre{cls}><code>{html.escape(chr(10).join(buf))}</code></pre>")
            continue

        if line.startswith("<") and re.match(r"^<[a-zA-Z][\w-]*[\s>/]", line):
            flush_paragraph(para)
            tag_match = re.match(r"^<([a-zA-Z][\w-]*)", line)
            tag = tag_match.group(1).lower() if tag_match else ""
            block: list[str] = [line]
            depth = line.count(f"<{tag}") - line.count(f"</{tag}>")
            i += 1
            while i < n and depth > 0:
                block.append(lines[i])
                depth += lines[i].count(f"<{tag}") - lines[i].count(f"</{tag}>")
                i += 1
            inner = "\n".join(block)
            if tag == "div":
                m = re.match(r"^<div[^>]*>\s*\n(.*?)\n\s*</div>\s*$", inner, re.DOTALL)
                if m:
                    open_line = block[0].rstrip()
                    rendered_inner = render_markdown(m.group(1))
                    out.append(f"{open_line}\n{rendered_inner}\n</div>")
                    continue
            out.append(inner)
            continue

        if re.match(r"^-{3,}\s*$", stripped):
            flush_paragraph(para)
            out.append("<hr />")
            i += 1
            continue

        m = re.match(r"^(#{1,6})\s+(.*)$", stripped)
        if m:
            flush_paragraph(para)
            level = len(m.group(1))
            text = m.group(2).strip()
            slug = slugify(text)
            out.append(f'<h{level} id="{slug}">{render_inline(text)}</h{level}>')
            i += 1
            continue

        if stripped.startswith(">"):
            flush_paragraph(para)
            buf2: list[str] = []
            while i < n and lines[i].strip().startswith(">"):
                buf2.append(lines[i].strip().lstrip(">").strip())
                i += 1
            inner = render_markdown("\n".join(buf2))
            out.append(f"<blockquote>{inner}</blockquote>")
            continue

        if re.match(r"^[-*]\s+", stripped):
            flush_paragraph(para)
            items = collect_list_items(lines, i, n, ordered=False)
            i, raw_items = items
            rendered = "".join(f"<li>{render_inline(it)}</li>" for it in raw_items)
            out.append(f"<ul>{rendered}</ul>")
            continue

        if re.match(r"^\d+\.\s+", stripped):
            flush_paragraph(para)
            items = collect_list_items(lines, i, n, ordered=True)
            i, raw_items = items
            rendered = "".join(f"<li>{render_inline(it)}</li>" for it in raw_items)
            out.append(f"<ol>{rendered}</ol>")
            continue

        if stripped.startswith("|") and stripped.endswith("|") and i + 1 < n:
            sep = lines[i + 1].strip()
            if re.match(r"^\|?\s*:?-{3,}.*\|", sep):
                flush_paragraph(para)
                header_cells = [c.strip() for c in stripped.strip("|").split("|")]
                i += 2
                rows: list[list[str]] = []
                while i < n and lines[i].strip().startswith("|") and lines[i].strip().endswith("|"):
                    rows.append([c.strip() for c in lines[i].strip().strip("|").split("|")])
                    i += 1
                head = "".join(f"<th>{render_inline(c)}</th>" for c in header_cells)
                body = "".join(
                    "<tr>" + "".join(f"<td>{render_inline(c)}</td>" for c in row) + "</tr>"
                    for row in rows
                )
                out.append(f"<table><thead><tr>{head}</tr></thead><tbody>{body}</tbody></table>")
                continue

        para.append(line)
        i += 1

    flush_paragraph(para)
    return "\n".join(out)


def slugify(text: str) -> str:
    s = text.lower()
    s = re.sub(r"[^\w\s-]", "", s, flags=re.UNICODE)
    s = re.sub(r"\s+", "-", s).strip("-")
    return s or "section"


# ---------------------------------------------------------------------------
# Site model
# ---------------------------------------------------------------------------


class Page:
    __slots__ = ("source", "rel_url", "title", "order", "html_body", "section")

    def __init__(self, source: Path, rel_url: str, title: str, order: int, body: str, section: str):
        self.source = source
        self.rel_url = rel_url
        self.title = title
        self.order = order
        self.html_body = body
        self.section = section


def collect_pages() -> list[Page]:
    pages: list[Page] = []
    for md in sorted(CONTENT.rglob("*.md")):
        raw = md.read_text(encoding="utf-8")
        meta, body = parse_front_matter(raw)
        rel = md.relative_to(CONTENT)
        if rel.name == "index.md":
            url = "/" if rel.parent == Path(".") else f"/{rel.parent.as_posix()}/"
        else:
            url = "/" + rel.with_suffix(".html").as_posix()
        title = meta.get("title") or first_heading(body) or rel.stem.replace("-", " ").title()
        order = int(meta.get("order", "100"))
        section = rel.parts[0] if len(rel.parts) > 1 else ""
        rendered = render_markdown(body)
        pages.append(Page(md, url, title, order, rendered, section))
    return pages


def first_heading(body: str) -> str | None:
    for line in body.splitlines():
        m = re.match(r"^#\s+(.*)$", line.strip())
        if m:
            return m.group(1).strip()
    return None


# ---------------------------------------------------------------------------
# Layout & sidebar
# ---------------------------------------------------------------------------

SIDEBAR_GROUPS = {
    "documentation": [
        ("start", "Start"),
        ("learn", "Learn"),
        ("query", "Query"),
        ("connect", "Connect"),
        ("manage", "Manage"),
        ("evaluate", "Evaluate"),
        ("project", "Project"),
    ],
}


def build_top_nav(pages: Iterable[Page], current: Page) -> str:
    entries: list[tuple[int, str, str, str, bool]] = []
    seen_targets: set[str] = set()

    home = next((p for p in pages if p.rel_url == "/"), None)
    if home:
        entries.append((home.order, home.title, home.rel_url, "Home", home is current))
        seen_targets.add(home.rel_url)

    sections: dict[str, Page | None] = {}
    for p in pages:
        if p.section and p.section not in sections:
            sections[p.section] = next(
                (q for q in pages if q.rel_url == f"/{p.section}/"), None
            )

    for section, index_page in sections.items():
        href = index_page.rel_url if index_page else f"/{section}/"
        if href in seen_targets:
            continue
        label = index_page.title if index_page else section.replace("-", " ").title()
        order = index_page.order if index_page else 100
        is_active = current.section == section
        entries.append((order, label, href, label, is_active))
        seen_targets.add(href)

    for p in pages:
        if "/" in p.rel_url.strip("/"):
            continue
        if p.rel_url in seen_targets:
            continue
        entries.append((p.order, p.title, p.rel_url, p.title, p is current))
        seen_targets.add(p.rel_url)

    entries.sort(key=lambda e: (e[0], e[1]))
    items: list[str] = []
    for _, _, href, label, active in entries:
        cls = "is-active" if active else ""
        items.append(f'<a class="nav-link {cls}" href="{href}">{html.escape(label)}</a>')
    return "\n".join(items)


def sidebar_group(page: Page) -> str:
    rel = page.source.relative_to(CONTENT)
    if page.section in SIDEBAR_GROUPS and len(rel.parts) > 2:
        return rel.parts[1]
    return ""


def sidebar_group_label(section: str, group: str) -> str:
    for key, label in SIDEBAR_GROUPS.get(section, []):
        if key == group:
            return label
    return group.replace("-", " ").title()


def sidebar_group_order(section: str, group: str) -> int:
    for idx, (key, _) in enumerate(SIDEBAR_GROUPS.get(section, [])):
        if key == group:
            return idx
    return len(SIDEBAR_GROUPS.get(section, []))


def render_sidebar_link(page: Page, current: Page) -> str:
    cls = "is-active" if page is current else ""
    return (
        f'<li><a class="sidebar-link {cls}" href="{page.rel_url}">'
        f"{html.escape(page.title)}</a></li>"
    )


def build_sidebar(pages: list[Page], current: Page) -> str:
    if not current.section:
        return ""
    section_pages = [p for p in pages if p.section == current.section]
    section_pages.sort(key=lambda p: (p.order, p.title))

    if current.section in SIDEBAR_GROUPS:
        section_index = [p for p in section_pages if sidebar_group(p) == ""]
        grouped: dict[str, list[Page]] = {}
        for p in section_pages:
            group = sidebar_group(p)
            if not group:
                continue
            grouped.setdefault(group, []).append(p)

        blocks: list[str] = []
        if section_index:
            index_items = "".join(render_sidebar_link(p, current) for p in section_index)
            blocks.append(f'<ul class="sidebar-list sidebar-root">{index_items}</ul>')

        for group in sorted(grouped, key=lambda key: sidebar_group_order(current.section, key)):
            group_pages = sorted(grouped[group], key=lambda p: (p.order, p.title))
            items = "".join(render_sidebar_link(p, current) for p in group_pages)
            label = html.escape(sidebar_group_label(current.section, group))
            is_open = " open" if any(p is current for p in group_pages) else ""
            blocks.append(
                f'<details class="sidebar-group"{is_open}>'
                f'<summary class="sidebar-heading">{label}</summary>'
                f'<ul class="sidebar-list">{items}</ul>'
                "</details>"
            )
        if not blocks:
            return ""
        return f'<nav class="sidebar">{"".join(blocks)}</nav>'

    items = [render_sidebar_link(p, current) for p in section_pages]
    if not items:
        return ""
    return (
        f'<nav class="sidebar"><ul class="sidebar-list">{"".join(items)}</ul></nav>'
    )


def render_layout(template: str, page: Page, top_nav: str, sidebar: str) -> str:
    body_class = "has-sidebar" if sidebar else "no-sidebar"
    return (
        template.replace("{{ title }}", html.escape(page.title))
        .replace("{{ top_nav }}", top_nav)
        .replace("{{ sidebar }}", sidebar)
        .replace("{{ content }}", page.html_body)
        .replace("{{ body_class }}", body_class)
    )


# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------


def build(out_dir: Path) -> None:
    if out_dir.exists():
        shutil.rmtree(out_dir)
    out_dir.mkdir(parents=True)

    template = read_validated_layout()

    pages = collect_pages()
    if not pages:
        print("warning: no markdown pages found in content/", file=sys.stderr)

    # Copy non-markdown content assets (e.g. benchmark JSON snapshots) so pages
    # can fetch them via stable URLs under /documentation/... .
    for asset in CONTENT.rglob("*"):
        if not asset.is_file() or asset.suffix == ".md":
            continue
        rel_asset = asset.relative_to(CONTENT)
        target_asset = out_dir / rel_asset
        target_asset.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(asset, target_asset)

    # Theme assets are copied after content assets so a generated content file
    # can never overwrite the global design system (`style.css`, JS, logos).
    copy_theme_assets(out_dir)

    for page in pages:
        top_nav = build_top_nav(pages, page)
        sidebar = build_sidebar(pages, page)
        rendered = render_layout(template, page, top_nav, sidebar)
        if page.rel_url.endswith("/"):
            target = out_dir / page.rel_url.strip("/") / "index.html"
        else:
            target = out_dir / page.rel_url.strip("/")
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(rendered, encoding="utf-8")
        print(f"  built {page.rel_url}")

    import json
    manifest = sorted({p.rel_url for p in pages})
    (out_dir / "manifest.json").write_text(
        json.dumps(manifest, indent=2, ensure_ascii=False),
        encoding="utf-8",
    )

    print(f"\ndone. {len(pages)} page(s) -> {out_dir}")


def read_validated_layout() -> str:
    style = (THEME / "style.css").read_text(encoding="utf-8")
    if THEME_STYLE_MARKER not in style:
        raise SystemExit(
            "theme guard failed: docs/theme/style.css is missing "
            f"{THEME_STYLE_MARKER}. Restore the studio theme before building."
        )

    template = (THEME / "layout.html").read_text(encoding="utf-8")
    if "/copy-code.js" not in template:
        raise SystemExit(
            "theme guard failed: docs/theme/layout.html must load /copy-code.js "
            "so generated code blocks keep syntax color and copy buttons."
        )
    return template


def copy_theme_assets(out_dir: Path) -> None:
    shutil.copy(THEME / "style.css", out_dir / "style.css")
    for asset in THEME.iterdir():
        if not asset.is_file():
            continue
        if asset.name in {"layout.html", "style.css"}:
            continue
        shutil.copy(asset, out_dir / asset.name)


_HREF = re.compile(r"""href=["']([^"']+)["']""")


def check_links(out_dir: Path) -> int:
    errors: list[str] = []
    html_files = sorted(out_dir.rglob("*.html"))
    for html_file in html_files:
        text = html_file.read_text(encoding="utf-8")
        for match in _HREF.finditer(text):
            href = html.unescape(match.group(1))
            target = local_href_target(out_dir, html_file, href)
            if target is None:
                continue
            if not target.exists():
                rel_source = html_file.relative_to(out_dir)
                errors.append(f"{rel_source}: broken link {href}")

    if errors:
        print("link check failed:", file=sys.stderr)
        for error in errors:
            print(f"  {error}", file=sys.stderr)
        return 1

    print(f"link check ok. {len(html_files)} html file(s)")
    return 0


def local_href_target(out_dir: Path, source: Path, href: str) -> Path | None:
    parsed = urlparse(href)
    if parsed.scheme or parsed.netloc:
        return None
    if parsed.path == "":
        return None
    if parsed.path.startswith(("mailto:", "tel:", "javascript:")):
        return None

    path = unquote(parsed.path)
    if path.startswith("/"):
        target = out_dir / path.lstrip("/")
    else:
        target = source.parent / path

    if path.endswith("/"):
        return target / "index.html"
    if target.suffix:
        return target
    return target / "index.html"


class _ReuseTCPServer(socketserver.TCPServer):
    allow_reuse_address = True


def serve(out_dir: Path, port: int = 8000) -> None:
    out_abs = str(out_dir.resolve())
    handler_cls = type(
        "DocsHandler",
        (http.server.SimpleHTTPRequestHandler,),
        {"__init__": lambda self, *a, **kw: http.server.SimpleHTTPRequestHandler.__init__(
            self, *a, directory=out_abs, **kw
        )},
    )
    with _ReuseTCPServer(("127.0.0.1", port), handler_cls) as httpd:
        print(f"serving {out_abs} at http://127.0.0.1:{port}")
        httpd.serve_forever()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT, help="output directory")
    parser.add_argument("--check-links", action="store_true", help="validate local links after building")
    parser.add_argument("--serve", action="store_true", help="serve after building")
    parser.add_argument("--port", type=int, default=8000)
    args = parser.parse_args()
    build(args.out)
    if args.check_links:
        result = check_links(args.out)
        if result:
            return result
    if args.serve:
        serve(args.out, args.port)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
