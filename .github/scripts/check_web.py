#!/usr/bin/env python3
"""Static integrity check for the `public/` landing page.

Verifies:
1. Required entry files exist (`index.html`, `style.css`, `main.js`).
2. All `.js` files under `public/` pass `node --check` syntax validation.
3. All relative `href` / `src` references inside HTML pages resolve to existing
   files/directories inside `public/` (except `docs/` mount links, which are
   verified against the assembled `dist-pages/docs/` tree by `docs/site/scripts/check-build.mjs`),
   and all local `#anchor` targets match an `id="..."` attribute on the target page
   (HTML comments ignored).
4. CSS `url(...)` references (excluding external/data URIs) resolve on disk.
"""

from __future__ import annotations

import pathlib
import re
import subprocess
import sys
from urllib.parse import unquote

ROOT = pathlib.Path(__file__).resolve().parents[2]
WEB_DIR = ROOT / "public"

COMMENT_RE = re.compile(r"<!--.*?-->", re.DOTALL)
ATTR_RE = re.compile(r"""(?:href|src)\s*=\s*["']([^"']+)["']""", re.IGNORECASE)
ID_RE = re.compile(r"""\bid\s*=\s*["']([^"']+)["']""", re.IGNORECASE)
CSS_URL_RE = re.compile(r"""url\(\s*["']?([^"')]+)["']?\s*\)""", re.IGNORECASE)

EXTERNAL_SCHEMES = ("http://", "https://", "mailto:", "tel:", "data:", "javascript:", "//")


def strip_comments(html: str) -> str:
    return COMMENT_RE.sub("", html)


def collect_ids(html_path: pathlib.Path) -> set[str]:
    text = strip_comments(html_path.read_text(encoding="utf-8"))
    return set(ID_RE.findall(text))


def main() -> int:
    errors: list[str] = []

    if not WEB_DIR.is_dir():
        print(f"ERROR: missing directory {WEB_DIR}", file=sys.stderr)
        return 1

    for required in ("index.html", "style.css", "main.js"):
        if not (WEB_DIR / required).is_file():
            errors.append(f"Missing required file: public/{required}")

    # 1. Syntax-check all JS files with Node.
    js_files = sorted(WEB_DIR.rglob("*.js"))
    for js_file in js_files:
        res = subprocess.run(
            ["node", "--check", str(js_file)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        if res.returncode != 0:
            errors.append(f"JS syntax check failed for {js_file.relative_to(ROOT)}:\n{res.stderr.strip()}")

    # 2. Check HTML relative assets, links, and fragment anchors.
    html_files = sorted(WEB_DIR.rglob("*.html"))
    ids_by_file = {path: collect_ids(path) for path in html_files}

    for html_file in html_files:
        rel_html = html_file.relative_to(ROOT)
        text = strip_comments(html_file.read_text(encoding="utf-8"))
        for ref in ATTR_RE.findall(text):
            ref = ref.strip()
            if not ref or ref.startswith(EXTERNAL_SCHEMES):
                continue

            path_part, _, fragment = ref.partition("#")
            path_part = path_part.split("?", 1)[0]

            if not path_part:
                if fragment and fragment not in ids_by_file[html_file]:
                    errors.append(f"{rel_html}: broken anchor '#{fragment}'")
                continue

            decoded = unquote(path_part)
            if decoded.startswith("/"):
                errors.append(f"{rel_html}: root-absolute path '{ref}' is not portable for static hosting")
                continue

            # Mounted Starlight docs routes are verified against `dist-pages/` in `docs` job.
            normalized_rel = decoded.lstrip("./")
            if normalized_rel == "docs" or normalized_rel.startswith("docs/"):
                continue

            target = (html_file.parent / decoded).resolve()
            try:
                target.relative_to(WEB_DIR.resolve())
            except ValueError:
                errors.append(f"{rel_html}: reference escapes public/: '{ref}'")
                continue

            if target.is_dir():
                target_index = target / "index.html"
                if not target_index.is_file():
                    errors.append(f"{rel_html}: directory link '{ref}' missing index.html")
                    continue
                target_html = target_index
            elif target.is_file():
                target_html = target if target.suffix.lower() == ".html" else None
            else:
                errors.append(f"{rel_html}: missing target '{ref}'")
                continue

            if fragment and target_html is not None:
                target_ids = ids_by_file.get(target_html)
                if target_ids is None:
                    target_ids = collect_ids(target_html)
                    ids_by_file[target_html] = target_ids
                if fragment not in target_ids:
                    errors.append(f"{rel_html}: broken cross-page anchor '{ref}'")

    # 3. Check relative CSS url(...) assets.
    css_files = sorted(WEB_DIR.rglob("*.css"))
    for css_file in css_files:
        rel_css = css_file.relative_to(ROOT)
        text = css_file.read_text(encoding="utf-8")
        for ref in CSS_URL_RE.findall(text):
            ref = ref.strip()
            if not ref or ref.startswith(EXTERNAL_SCHEMES) or ref.startswith("#"):
                continue
            decoded = unquote(ref.split("?", 1)[0].split("#", 1)[0])
            target = (css_file.parent / decoded).resolve()
            if not target.is_file():
                errors.append(f"{rel_css}: missing CSS asset '{ref}'")

    if errors:
        print("public/ static check failed:", file=sys.stderr)
        for err in errors:
            print(f"  - {err}", file=sys.stderr)
        return 1

    print(
        f"Verified public/: {len(html_files)} HTML pages, {len(css_files)} CSS files, "
        f"{len(js_files)} JS files (syntax, relative assets, and anchors OK)."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
