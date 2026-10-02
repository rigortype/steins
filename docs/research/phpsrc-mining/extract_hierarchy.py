#!/usr/bin/env python3
"""Extract class/interface/enum declarations with extends/implements from php-src stubs."""
import json, os, re, subprocess, sys, glob

ROOT = os.environ.get("PHP_SRC_ROOT", os.path.expanduser("~/local/src/php-src"))
stubs = sorted(glob.glob(os.path.join(ROOT, "**", "*.stub.php"), recursive=True))

# Match a type declaration keyword sequence. We scan the file, and when we find a
# line starting (ignoring leading whitespace) with modifiers + class/interface/enum,
# we accumulate text until the first '{'.
DECL_RE = re.compile(
    r'^(?P<mods>(?:abstract\s+|final\s+|readonly\s+)*)'
    r'(?P<kind>class|interface|enum)\s+'
    r'(?P<name>[A-Za-z_\\][A-Za-z0-9_\\]*)'
    r'(?P<rest>.*)$', re.DOTALL)

def resolve(ref, cur_ns):
    """Resolve a name in an `extends`/`implements` list the way PHP does: a leading
    backslash is fully qualified, anything else is relative to the current namespace
    (no stub uses `use`)."""
    ref = ref.strip()
    if ref.startswith('\\') or not cur_ns:
        return ref.lstrip('\\')
    return cur_ns + '\\' + ref

def parse_file(path):
    with open(path, encoding='utf-8', errors='replace') as fh:
        return parse_lines(fh.readlines(), os.path.relpath(path, ROOT))

def parse_lines(lines, relpath):
    out = []
    i = 0
    n = len(lines)
    cur_ns = ""
    while i < n:
        raw = lines[i]
        stripped = raw.lstrip()
        # A `namespace` statement: `namespace X {`, `namespace X;`, `namespace {`, and
        # the same with the brace on a following line (`namespace Random` then `{`,
        # which random.stub.php and php_dom.stub.php use). Accumulate lines up to the
        # `{` or `;` so the name is read whatever the layout.
        if re.match(r'namespace\b(?!\\)', stripped):
            header = stripped
            j = i
            while not re.search(r'[{;]', header) and j + 1 < n:
                j += 1
                header += ' ' + lines[j].strip()
            nm = re.match(r'namespace\s*([A-Za-z_\\][A-Za-z0-9_\\]*)?\s*[{;]', header)
            if nm:
                cur_ns = (nm.group(1) or "").rstrip('\\')
                i = j + 1
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
                extends = [resolve(x, cur_ns) for x in em.group(1).split(',') if x.strip()]
            im = re.search(r'\bimplements\s+(.+)$', rest)
            if im:
                implements = [resolve(x, cur_ns) for x in im.group(1).split(',') if x.strip()]
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
                'file': relpath,
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

# A relative parent name that resolves to no declaration, while its global tail does,
# is the stub's own slip (`class OpensslException extends Exception` inside
# `namespace Openssl`, whose arginfo registers it with `zend_ce_exception`): the
# engine's parent is the global class, so record that and say so.
for d in seen.values():
    for field in ('extends', 'implements'):
        fixed = []
        for ref in d[field]:
            tail = ref.rsplit('\\', 1)[-1]
            if ref.lower() not in seen and ref != tail and tail.lower() in seen \
                    and '\\' not in seen[tail.lower()]['name']:
                print(f"# {d['name']}: `{ref}` is no declaration; recorded as `{tail}`",
                      file=sys.stderr)
                ref = tail
            fixed.append(ref)
        d[field] = fixed

# Which rows the pinned release declares. The stubs above are php-src's development
# branch; the pinned PHP is an older minor, so a row may name a class that release does not
# have (`Io\Poll\PollException`, `StreamException`). Whether it does is a fact about php-src
# at the release, not about the machine the miner runs on, so it is read from the release's
# own stubs: the same parser, run over every `*.stub.php` at the tag `PINNED_TAG` (default:
# the newest stable `php-<minor>.<n>` tag the checkout has, for the minor of `PHP_BIN`). A
# row whose class is not declared there is marked `absent_on_pinned = true`, and the catalog
# refuses to treat it as an engine class (#871). An extension a build lacks has nothing to do
# with it: `EnchantBroker` is declared at the tag whether or not this PHP has ext-enchant.
PHP_BIN = os.environ.get("PHP_BIN", "php")

def run(cmd, **kw):
    try:
        return subprocess.run(cmd, capture_output=True, text=True, check=True, **kw).stdout
    except (OSError, subprocess.CalledProcessError) as e:
        sys.exit(f"extract_hierarchy.py: `{' '.join(cmd[:3])}` failed (PHP_BIN={PHP_BIN}, "
                 f"PHP_SRC_ROOT={ROOT}): {e}")

php_version = run([PHP_BIN, "-r", "echo PHP_VERSION;"]).strip()
minor = ".".join(php_version.split(".")[:2])
pinned_tag = os.environ.get("PINNED_TAG")
if not pinned_tag:
    tags = [t for t in run(["git", "tag", "-l", f"php-{minor}.*"], cwd=ROOT).split()
            if re.fullmatch(r"php-\d+\.\d+\.\d+", t)]
    if not tags:
        sys.exit(f"extract_hierarchy.py: no php-{minor}.N tag in {ROOT} (set PINNED_TAG)")
    pinned_tag = max(tags, key=lambda t: [int(x) for x in t[4:].split(".")])
pinned_commit = run(["git", "rev-parse", pinned_tag + "^{commit}"], cwd=ROOT).strip()
tag_names = set()
for path in run(["git", "ls-tree", "-r", "--name-only", pinned_tag], cwd=ROOT).split("\n"):
    if path.endswith(".stub.php"):
        text = run(["git", "show", f"{pinned_tag}:{path}"], cwd=ROOT)
        tag_names.update(d['name'].lower() for d in parse_lines(text.splitlines(True), path))

TEST_PREFIXES = ("ext/zend_test/", "ext/skeleton/", "ext/dl_test/", "sapi/")
production = [d for d in seen.values() if not d['file'].startswith(TEST_PREFIXES)]
absent = {d['name'] for d in production if d['name'].lower() not in tag_names}
print(f"# {pinned_tag} ({pinned_commit[:10]}): {len(absent)} of {len(production)} rows are not "
      f"declared: {sorted(absent)}", file=sys.stderr)

# Sanity check against the local PHP, never a source: `class_exists`-family, autoload off.
# A row the tag declares, whose extension is loaded here and which this PHP lacks, or a row
# the tag lacks that this PHP declares, says the tag, the parse or the build is off.
PROBE = (
    '$n = json_decode(stream_get_contents(STDIN)); $o = [];'
    'foreach ($n as [$x, $e]) { $o[] = [class_exists($x, false) || interface_exists($x, false)'
    ' || enum_exists($x, false) || trait_exists($x, false), extension_loaded($e)]; }'
    'echo json_encode($o);'
)

def ext_of(source):
    parts = source.split('/')
    if parts[0] == 'ext':
        return 'zend opcache' if parts[1] == 'opcache' else parts[1]
    return 'core'

local = json.loads(run([PHP_BIN, "-r", PROBE],
                       input=json.dumps([[d['name'], ext_of(d['file'])] for d in production])))
for d, (here, ext_loaded) in zip(production, local):
    if d['name'] in absent and here:
        print(f"# WARNING: `{d['name']}` is not at {pinned_tag} but PHP {php_version} declares it",
              file=sys.stderr)
    if d['name'] not in absent and not here and ext_loaded:
        print(f"# WARNING: `{d['name']}` is at {pinned_tag} and `{ext_of(d['file'])}` is loaded, "
              f"but PHP {php_version} lacks it", file=sys.stderr)

print(f"# total declarations parsed: {len(all_decls)}, unique names: {len(seen)}", file=sys.stderr)
if dups:
    print(f"# duplicate names: {dups}", file=sys.stderr)

# Emit TOML
def toml_list(xs):
    return "[" + ", ".join("'%s'" % x for x in xs) + "]"

rows = sorted(seen.values(), key=lambda d: (d['file'], d['line']))
print('# hierarchy.toml — builtin class/interface/enum hierarchy mined from php-src stubs')
print('# php-src commit: 6bc7c26cf67a9480b5ef9d6191aebe87fa931183 (Thu Jul 9 2026)')
print(f'# Pinned release: {pinned_tag} ({pinned_commit}). A row whose class is not declared')
print('# by that release\'s own stubs carries `absent_on_pinned = true`, and the catalog does')
print('# not treat it as an engine class. Extensions a build lacks do not enter into it.')
print('# Names preserve declared casing; Steins lowercases at its seam.')
print('# Namespaced names are fully-qualified (no leading backslash).')
print('# Test-only extensions (ext/zend_test, ext/skeleton, ext/dl_test, sapi/*) are EXCLUDED.')
print(f'# Total production declarations: {sum(1 for d in rows if not d["file"].startswith(TEST_PREFIXES))}')
print()
print(f"pinned_tag = '{pinned_tag}'")
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
    if d['name'] in absent:
        print('absent_on_pinned = true')
    print(f"source = '{d['file']}:{d['line']}'")
    print()
