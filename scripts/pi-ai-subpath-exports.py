#!/usr/bin/env python3
"""Runtime export names of pi-ai's package.json subpaths, from the TypeScript sources.

Usage: scripts/pi-ai-subpath-exports.py <pi checkout> > crates/pi-coding-agent/tests/fixtures/pi-ai-subpath-exports.json

Reads `packages/ai/package.json` `exports` and, for every subpath (wildcards expanded over
`packages/ai/src`), collects the value exports of the TS module: declarations, `export { .. }`
lists and `export * from` re-exports (followed). Type-only exports are erased by TypeScript and
skipped. `.` and `./compat` are left out: `@earendil-works/pi-ai` serves the compat superset and
is checked by `js/EXPORTS.md` / the root export tests.
"""
import json
import os
import re
import sys

DECL = re.compile(
    r"^export\s+(?:declare\s+)?(default\s+)?(?:async\s+)?(?:abstract\s+)?(?:function\*?|const|let|var|class|enum)\s+([A-Za-z_$][\w$]*)",
    re.M,
)
DESTRUCT = re.compile(r"^export\s+(?:const|let|var)\s+\{([^}]*)\}\s*=", re.M)
LIST = re.compile(r"^export\s+(type\s+)?\{([^}]*)\}", re.M)
STAR = re.compile(r"^export\s+\*\s+(?:as\s+([\w$]+)\s+)?from\s+[\"']([^\"']+)[\"']", re.M)


def resolve(base, spec):
    path = os.path.normpath(os.path.join(os.path.dirname(base), spec))
    for candidate in (path, re.sub(r"\.js$", ".ts", path), path + ".ts", os.path.join(path, "index.ts")):
        if candidate.endswith(".ts") and os.path.isfile(candidate):
            return candidate
    raise SystemExit(f"unresolved {spec} from {base}")


def exports(path, cache={}):
    if path in cache:
        return cache[path]
    cache[path] = set()
    with open(path, encoding="utf-8") as f:
        text = f.read()
    names = set()
    for m in DECL.finditer(text):
        names.add("default" if m.group(1) else m.group(2))
    for m in DESTRUCT.finditer(text):
        names.update(p.split(":")[-1].strip() for p in m.group(1).split(",") if p.strip())
    for m in LIST.finditer(text):
        if m.group(1):
            continue
        for part in m.group(2).split(","):
            part = part.strip()
            if part and not part.startswith("type "):
                names.add(part.split(" as ")[-1].strip())
    for m in STAR.finditer(text):
        if m.group(1):
            names.add(m.group(1))
        elif m.group(2).startswith("."):
            names |= exports(resolve(path, m.group(2))) - {"default"}
    cache[path] = names
    return names


def main():
    pkg = os.path.join(sys.argv[1], "packages", "ai")
    with open(os.path.join(pkg, "package.json"), encoding="utf-8") as f:
        export_map = json.load(f)["exports"]
    src = os.path.join(pkg, "src")
    out = {}
    for subpath, target in export_map.items():
        if subpath in (".", "./compat"):
            continue
        dist = target["import"]
        prefix, _, suffix = dist.partition("*")
        source_prefix = os.path.join(pkg, prefix.replace("./dist/", "src/"))
        if not _:
            out[subpath] = sorted(exports(source_prefix[: -len(".js")] + ".ts"))
            continue
        for root, dirs, files in os.walk(source_prefix):
            dirs[:] = [d for d in dirs if d != "data"]
            for name in files:
                if not name.endswith(".ts") or name.endswith(".d.ts"):
                    continue
                rel = os.path.relpath(os.path.join(root, name), source_prefix)[: -len(".ts")]
                out[subpath.replace("*", rel)] = sorted(exports(os.path.join(root, name)))
    json.dump(out, sys.stdout, indent="\t", sort_keys=True)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
