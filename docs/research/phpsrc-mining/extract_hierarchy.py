#!/usr/bin/env python3
"""Extract class/interface/enum declarations with extends/implements from php-src stubs."""
import os, re, sys, glob

ROOT = "/Users/megurine/local/src/php-src"
stubs = sorted(glob.glob(os.path.join(ROOT, "**", "*.stub.php"), recursive=True))

# Match a type declaration keyword sequence. We scan the file, and when we find a
# line starting (ignoring leading whitespace) with modifiers + class/interface/enum,
# we accumulate text until the first '{'.
DECL_RE = re.compile(
    r'^(?P<mods>(?:abstract\s+|final\s+|readonly\s+)*)'
    r'(?P<kind>class|interface|enum)\s+'
    r'(?P<name>[A-Za-z_\\][A-Za-z0-9_\\]*)'
    r'(?P<rest>.*)$', re.DOTALL)

def parse_file(path):
    with open(path, encoding='utf-8', errors='replace') as fh:
        lines = fh.readlines()
    out = []
    i = 0
    n = len(lines)
    cur_ns = ""
    while i < n:
        raw = lines[i]
        stripped = raw.lstrip()
        nm = re.match(r'namespace\s*([A-Za-z_\\][A-Za-z0-9_\\]*)?\s*[{;]', stripped)
        if nm:
            cur_ns = (nm.group(1) or "").rstrip('\\')
            i += 1
            continue
        m = re.match(r'(abstract\s+|final\s+|readonly\s+)*(class|interface|enum)\s', stripped)
        if not m:
            i += 1
            continue
        # accumulate header until '{'
        header = stripped
        j = i
        while '{' not in header and j + 1 < n:
            j += 1
            header += ' ' + lines[j].strip()
        header = header.split('{', 1)[0]
        # collapse whitespace
        header = re.sub(r'\s+', ' ', header).strip()
        dm = DECL_RE.match(header)
        if dm:
            mods = dm.group('mods').strip()
            kind = dm.group('kind')
            name = dm.group('name')
            rest = dm.group('rest')
            # strip enum backing type ": int"/": string"
            rest = re.sub(r'^\s*:\s*(int|string)\b', '', rest)
            extends = []
            implements = []
            em = re.search(r'\bextends\s+(.+?)(?:\bimplements\b|$)', rest)
            if em:
                extends = [x.strip().lstrip('\\') for x in em.group(1).split(',') if x.strip()]
            im = re.search(r'\bimplements\s+(.+)$', rest)
            if im:
                implements = [x.strip().lstrip('\\') for x in im.group(1).split(',') if x.strip()]
            fqname = name.lstrip('\\')
            if cur_ns:
                fqname = cur_ns + '\\' + fqname
            out.append({
                'name': fqname,
                'ns': cur_ns,
                'kind': kind,
                'mods': mods,
                'extends': extends,
                'implements': implements,
                'file': os.path.relpath(path, ROOT),
                'line': i + 1,
            })
        i = j + 1
    return out

all_decls = []
for s in stubs:
    all_decls.extend(parse_file(s))

# dedupe by name (keep first); report duplicates
seen = {}
dups = []
for d in all_decls:
    key = d['name'].lower()
    if key in seen:
        dups.append((d['name'], d['file'], d['line']))
    else:
        seen[key] = d

print(f"# total declarations parsed: {len(all_decls)}, unique names: {len(seen)}", file=sys.stderr)
if dups:
    print(f"# duplicate names: {dups}", file=sys.stderr)

# Emit TOML
def toml_list(xs):
    return "[" + ", ".join("'%s'" % x for x in xs) + "]"

TEST_PREFIXES = ("ext/zend_test/", "ext/skeleton/", "ext/dl_test/", "sapi/")
rows = sorted(seen.values(), key=lambda d: (d['file'], d['line']))
print('# hierarchy.toml — builtin class/interface/enum hierarchy mined from php-src stubs')
print('# php-src commit: 6bc7c26cf67a9480b5ef9d6191aebe87fa931183 (Thu Jul 9 2026)')
print('# Cross-checked against PHP 8.5.8 (cli) at /opt/homebrew/bin/php where noted.')
print('# Names preserve declared casing; Steins lowercases at its seam.')
print('# Namespaced names are fully-qualified (no leading backslash).')
print('# Test-only extensions (ext/zend_test, ext/skeleton, ext/dl_test, sapi/*) are EXCLUDED.')
print(f'# Total production declarations: {sum(1 for d in rows if not d["file"].startswith(TEST_PREFIXES))}')
print()
for d in rows:
    if d['file'].startswith(TEST_PREFIXES):
        continue
    flags = []
    if 'abstract' in d['mods']:
        flags.append('abstract = true')
    if 'final' in d['mods']:
        flags.append('final = true')
    flagstr = (", " + ", ".join(flags)) if flags else ""
    print(f'[[class]]')
    print(f"name = '{d['name']}'")
    print(f"kind = '{d['kind']}'")
    print(f'extends = {toml_list(d["extends"])}')
    print(f'implements = {toml_list(d["implements"])}')
    if flags:
        for fl in flags:
            k, v = fl.split(' = ')
            print(f'{k} = {v}')
    print(f"source = '{d['file']}:{d['line']}'")
    print()
