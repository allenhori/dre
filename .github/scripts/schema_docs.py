#!/usr/bin/env python3
"""Generate the YAML reference pages in docs/ from the JSON Schemas in docs/schemas/.

    schema_docs.py generate   write docs/reference-*.md
    schema_docs.py check      fail if the pages in docs/ differ from what the schemas generate

The schemas' descriptions are the single source for the reference; edit them, never the pages.
"""
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCHEMAS = ROOT / "docs" / "schemas"
DOCS = ROOT / "docs"
URL = "https://getdre.com/schemas"

# schema file -> page slug, title, the file as users know it, sidebar order
PAGES = [
    ("project", "reference-project", "dre_project.yml", "dre_project.yml", 22,
     "Every key of dre_project.yml, the project file."),
    ("report", "reference-report", "Report YAML", "a `.yml` file under `reports/`", 23,
     "Every key of a report YAML file: queries, output, destinations, Sets and templates."),
    ("sets", "reference-sets", "sets.yml", "`sets.yml`, or any YAML file of Sets", 24,
     "Every key of a Sets file."),
    ("schedules", "reference-schedules", "schedules.yml", "`schedules.yml`", 25,
     "Every key of the schedules file."),
    ("timings", "reference-timings", "timings.yml", "`timings.yml`", 26,
     "Every key of the timings file: named timings schedules share."),
    ("profiles", "reference-profiles", "profiles.yml", "`profiles.yml`", 27,
     "Every key of profiles.yml: connections, destinations and their targets."),
    ("sources", "reference-sources", "Sources", "any project YAML file with a top-level `sources:` key, e.g. `sources/<name>.yml`", 27.5,
     "Every key of a sources declaration: sources, tables and columns, as in dbt."),
    ("dependencies", "reference-dependencies", "dependencies.yml", "`dependencies.yml` or `packages.yml`", 28,
     "Every key of dependencies.yml: plugin packages and macro packages."),
    ("lookup", "reference-lookups", "Lookup config", "`lookups/<name>.yml`, next to the lookup's data file", 29,
     "Every key of a lookup's config file."),
]


class Doc:
    def __init__(self, name):
        self.name = name
        self.root = json.loads((SCHEMAS / f"{name}.schema.json").read_text())
        self.cache = {name: self.root}

    def load(self, f):
        if f not in self.cache:
            self.cache[f] = json.loads((SCHEMAS / f"{f}.schema.json").read_text())
        return self.cache[f]

    def resolve(self, node, home):
        """Follow `$ref`s; returns (schema, home file, ref'd) with the node's own description kept."""
        desc = node.get("description")
        ref = False
        while "$ref" in node:
            ref = True
            target = node["$ref"]
            file, _, ptr = target.partition("#")
            if file:
                home = file.replace(".schema.json", "")
            doc = self.load(home)
            node = {**doc_pointer(doc, ptr), **{k: v for k, v in node.items() if k != "$ref"}}
        if desc:
            node = {**node, "description": desc}
        return node, home


def doc_pointer(doc, ptr):
    cur = doc
    for part in [p for p in ptr.split("/") if p]:
        cur = cur[part]
    return cur


def esc(text):
    return " ".join(str(text).split()).replace("|", "\\|")


def other_file(doc, node, home):
    """The schema file a `$ref` points into, when it isn't the one being documented."""
    f = node.get("$ref", "").partition("#")[0].replace(".schema.json", "")
    return f if f and f != doc.name else None


def type_of(doc, node, home):
    if "x-doc-type" in node:
        return node["x-doc-type"]
    foreign = other_file(doc, node, home)
    if foreign:
        slug = next(p[1] for p in PAGES if p[0] == foreign)
        return f"map, as in [the {foreign} reference]({slug}.md)"
    node, home = doc.resolve(node, home)
    if "enum" in node:
        return " or ".join(f"`{v}`" for v in node["enum"])
    if "oneOf" in node:
        parts = []
        for alt in node["oneOf"]:
            t = type_of(doc, alt, home)
            if t not in parts:
                parts.append(t)
        return " or ".join(parts)
    t = node.get("type")
    if isinstance(t, list):
        return " or ".join(t)
    if t == "array":
        return "list of " + type_of(doc, node.get("items", {}), home)
    if t == "object":
        return "map" if "properties" not in node else "map (see below)"
    return t or "any"


def candidates(doc, node, home):
    """Every object schema inside `node`: through `$ref`s, `oneOf` alternatives and list items."""
    if other_file(doc, node, home):
        return []
    node, home = doc.resolve(node, home)
    found = [(node, home)] if "properties" in node else []
    for alt in node.get("oneOf", []):
        found += candidates(doc, alt, home)
    if node.get("type") == "array" and "items" in node:
        found += candidates(doc, node["items"], home)
    return found


def tables(doc, node, home, path, out, seen):
    """Collect one table per object schema reachable from `node`."""
    for c, h in candidates(doc, node, home):
        key = (h, tuple(c["properties"]))
        if key in seen:
            continue
        seen.add(key)
        out.append((path, c, h))
        for name, sub in c["properties"].items():
            suffix = "[]" if is_list(doc, sub, h) else ""
            tables(doc, sub, h, f"{path}.{name}{suffix}" if path else f"{name}{suffix}", out, seen)
    resolved, h = doc.resolve(node, home)
    ap = resolved.get("additionalProperties")
    if isinstance(ap, dict) and path:
        key = ("ap", h, json.dumps(ap, sort_keys=True))
        if key not in seen:
            seen.add(key)
            tables(doc, ap, h, f"{path}.<name>", out, seen)


def is_list(doc, node, home):
    node, home = doc.resolve(node, home)
    if node.get("type") == "array":
        return True
    return any(doc.resolve(a, home)[0].get("type") == "array" for a in node.get("oneOf", []))


def render_table(doc, schema, home):
    required = set(schema.get("required", []))
    rows = ["| Key | Type | Default | Description |", "|---|---|---|---|"]
    for name, sub in schema["properties"].items():
        resolved, h = doc.resolve(sub, home)
        default = resolved.get("default")
        shown = "" if default is None else f"`{json.dumps(default) if not isinstance(default, str) else default}`"
        req = " (required)" if name in required else ""
        rows.append(f"| `{name}`{req} | {esc(type_of(doc, sub, home))} | {shown} | {esc(resolved.get('description', ''))} |")
    if schema.get("additionalProperties") is True:
        rows.append("| _other keys_ | | | Options of the plugin that handles this block; see [Plugins](plugins.md). |")
    return "\n".join(rows)


def generate_page(name, slug, title, where, order, description):
    doc = Doc(name)
    root = doc.root
    version = root.get("x-dre-schema-version", "")
    lines = [
        "---",
        f"title: {json.dumps(title + ' reference' if name != 'project' else 'dre_project.yml reference')}",
        f"description: {json.dumps(description)}",
        "sidebar:",
        f"  order: {order}",
        "---",
        "",
        f"# {title if name == 'project' else title} reference",
        "",
        "<!-- Generated from docs/schemas by .github/scripts/schema_docs.py. Edit the schema, not this page. -->",
        "",
        root.get("description", ""),
        "",
        f"Where: {where}.",
        "",
        "For editor autocomplete and validation, add this as the first line of the file "
        "([editor setup](editor-setup.md)):",
        "",
        "```yaml",
        f"# yaml-language-server: $schema={URL}/v{version}/{name}.schema.json",
        "```",
        "",
    ]
    found = []
    top = dict(root)
    top.pop("$defs", None)
    if root.get("type") == "array":
        lines += ["The file is a list; each entry has these keys.", ""]
        tables(doc, root["items"], name, "", found, set())
    elif "properties" not in root and isinstance(root.get("additionalProperties"), dict):
        lines += ["The file maps names to entries; each entry has these keys.", ""]
        tables(doc, root["additionalProperties"], name, "", found, set())
    else:
        tables(doc, top, name, "", found, set())
    for path, schema, home in found:
        if path:
            lines += [f"## `{path}`", "", schema.get("description", ""), ""]
        else:
            lines += ["## Keys", ""]
        lines += [render_table(doc, schema, home), ""]
    return "\n".join(lines).rstrip() + "\n"


def pages():
    return {f"{slug}.md": generate_page(name, slug, title, where, order, desc)
            for name, slug, title, where, order, desc in PAGES}


def main():
    cmd = sys.argv[1] if len(sys.argv) > 1 else "check"
    out = pages()
    if cmd == "generate":
        for f, text in out.items():
            (DOCS / f).write_text(text)
        print(f"wrote {len(out)} pages")
        return 0
    stale = [f for f, text in out.items() if not (DOCS / f).is_file() or (DOCS / f).read_text() != text]
    if stale:
        print("these pages are out of date; run `python3 .github/scripts/schema_docs.py generate`:", *stale, sep="\n  ")
        return 1
    print("reference pages are up to date")
    return 0


if __name__ == "__main__":
    sys.exit(main())
