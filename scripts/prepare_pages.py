#!/usr/bin/env python3
"""Add Glomeris' public identity metadata to an assembled Pages artifact."""
from __future__ import annotations

import argparse
import html
import re
import xml.etree.ElementTree as ET
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import quote, urlparse


BASE_URL = "https://chisanan232.github.io/glomeris/"
CANONICAL = re.compile(r"<link\b(?=[^>]*\brel=[\"'][^\"']*\bcanonical\b)[^>]*>", re.IGNORECASE)
BASE_TAG = re.compile(r"<base\b[^>]*>", re.IGNORECASE)
RUSTDOC_BACK_LINK = re.compile(r"(?P<prefix>\bhref=)(?P<quote>[\"'])javascript:void\(0\)(?P=quote)")
RUSTDOC_LINE_RANGE = re.compile(
    r"(?P<prefix>\bhref=[\"'][^\"']*#)(?P<start>\d+)-\d+(?P<quote>[\"'])"
)
RUSTDOC_TRAIT_IMPL = re.compile(
    r"<script\b(?=[^>]*\bsrc=(?P<quote>[\"'])(?P<src>[^\"']*trait\.impl/[^\"']+\.js)(?P=quote))",
    re.IGNORECASE,
)


class _IndexingParser(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.noindex = False

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = {key.lower(): value or "" for key, value in attrs}
        if tag.lower() == "meta" and values.get("name", "").lower() == "robots":
            directives = re.split(r"[\s,]+", values.get("content", "").lower())
            self.noindex = self.noindex or "noindex" in directives


def _public_url(relative: Path) -> str:
    path = relative.as_posix()
    if path == "index.html":
        path = ""
    elif path.endswith("/index.html"):
        path = path.removesuffix("index.html")
    return BASE_URL + quote(path, safe="/")


def _is_noindex(document: str) -> bool:
    parser = _IndexingParser()
    parser.feed(document)
    parser.close()
    return parser.noindex


def _with_canonical(document: str, canonical_url: str) -> str:
    tag = f'<link rel="canonical" href="{html.escape(canonical_url, quote=True)}">'
    matches = list(CANONICAL.finditer(document))
    if len(matches) > 1:
        raise ValueError("page contains more than one canonical link")
    if matches:
        return CANONICAL.sub(tag, document, count=1)
    head_end = document.lower().find("</head>")
    if head_end >= 0:
        return document[:head_end] + f"    {tag}\n" + document[head_end:]
    doctype_end = document.find(">") + 1 if document.lower().startswith("<!doctype") else 0
    return document[:doctype_end] + f"\n<head>{tag}</head>" + document[doctype_end:]


def _prepare_not_found(document: str) -> str:
    base = '<base href="/glomeris/">'
    if BASE_TAG.search(document):
        document = BASE_TAG.sub(base, document, count=1)
    else:
        head_end = document.lower().find("</head>")
        if head_end < 0:
            raise ValueError("404.html has no head for its project-path base")
        document = document[:head_end] + f"    {base}\n" + document[head_end:]
    if not _is_noindex(document):
        head_end = document.lower().find("</head>")
        document = document[:head_end] + '    <meta name="robots" content="noindex">\n' + document[head_end:]
    return CANONICAL.sub("", document)


def _prepare_rustdoc(document: str) -> str:
    document = RUSTDOC_BACK_LINK.sub(r"\g<prefix>\g<quote>./index.html\g<quote>", document)
    return RUSTDOC_LINE_RANGE.sub(r"\g<prefix>\g<start>\g<quote>", document)


def _ensure_rustdoc_support(page: Path, root: Path, document: str) -> None:
    for match in RUSTDOC_TRAIT_IMPL.finditer(document):
        parsed = urlparse(match.group("src"))
        if parsed.scheme or parsed.netloc:
            continue
        support = (page.parent / parsed.path).resolve()
        support.relative_to(root)
        if not support.exists():
            support.parent.mkdir(parents=True, exist_ok=True)
            support.write_text("// No public implementors.\n", encoding="utf-8")


def prepare(root: Path) -> int:
    root = root.resolve(strict=True)
    pages: list[str] = []
    for page in sorted(root.rglob("*.html")):
        page.resolve(strict=True).relative_to(root)
        relative = page.relative_to(root)
        document = page.read_text(encoding="utf-8")
        if relative == Path("404.html"):
            page.write_text(_prepare_not_found(document), encoding="utf-8")
            continue
        if relative.parts[0] == "rustdoc":
            document = _prepare_rustdoc(document)
            _ensure_rustdoc_support(page, root, document)
        if _is_noindex(document):
            continue
        public_url = _public_url(relative)
        page.write_text(_with_canonical(document, public_url), encoding="utf-8")
        pages.append(public_url)

    pages.sort(key=lambda public_url: (public_url != BASE_URL, public_url))
    urlset = ET.Element("urlset", xmlns="http://www.sitemaps.org/schemas/sitemap/0.9")
    for public_url in pages:
        url = ET.SubElement(urlset, "url")
        ET.SubElement(url, "loc").text = public_url
    ET.ElementTree(urlset).write(root / "sitemap.xml", encoding="utf-8", xml_declaration=True)
    (root / "robots.txt").write_text(
        f"User-agent: *\nAllow: /\nSitemap: {BASE_URL}sitemap.xml\n",
        encoding="utf-8",
    )
    return len(pages)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifact_root", type=Path)
    args = parser.parse_args()
    count = prepare(args.artifact_root)
    print(f"Prepared {count} indexable Pages documents")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
