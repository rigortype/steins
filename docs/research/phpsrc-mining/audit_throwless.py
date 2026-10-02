#!/usr/bin/env python3
"""Audit `THROWLESS_NAMES` (crates/steins-catalog/src/knowledge.rs) against php-src.

For each name on the table, at the pinned php-src checkout:

1. resolve the PHP name to the C function that runs it, through the generated
   `*_arginfo.h` entry tables (`ZEND_FE`, and `ZEND_RAW_FENTRY` which is what
   `ZEND_FALIAS` expands to, so `join` -> `zif_implode`);
2. find that function's definition: `PHP_FUNCTION`/`ZEND_FUNCTION`/
   `*_NAMED_FUNCTION`, and the `FileFunction(PHP_FN(name), FS_*)` macro family
   in ext/standard/filestat.c, which defines `file_exists`, `is_dir`, ... through
   one macro body;
3. walk the call graph from the body (a regex reading of every C function in the
   tree, comments and string literals masked), and list every *raise primitive*
   the walk reaches (`zend_throw_error`, `zend_value_error`,
   `zend_argument_value_error`, `zend_throw_exception*`, ...) with the C function
   that calls it, its line, its exception class and its message;
4. classify the name.

The classification is `none` when no primitive is reached; `argument-checking`
when every primitive reached is one php-src raises for the *type or count* of an
argument (the engine's parameter parsing, `zend_argument_type_error`, a
`TypeError` for an illegal offset or an unusable resource); and otherwise the
disposition the auditor recorded in `DISPOSITIONS` below, with the reason. A name
with a primitive and no recorded disposition is `unreviewed`, and the audit
refuses to call it clean.

The classes (ADR-0099 §3.3, §7.1):

* `none`: the body reaches no raise primitive.
* `argument-checking`: whatever is raised depends on an argument's type, count
  or a resource's validity, which the throw lane sets aside for every builtin.
* `destructor-hazard`: the only raise needs user code (a destructor) to have run
  inside the call and changed what the call is working on; §7.1 leaves
  destructors open for every `unset` and reassignment alike.
* `needs-row`: a call can raise for an argument *value* its parameter types
  admit. A name in this class does not belong on the table.
* `unreviewed`: the walk found a primitive and no disposition covers it.

Usage:  audit_throwless.py [--php-src DIR] [--out FILE]

Prints the note (a Markdown table) to stdout, or writes it to FILE. The php-src
checkout is `$PHP_SRC_ROOT` or `~/local/src/php-src`, and must be at PINNED.
"""
import argparse
import os
import re
import subprocess
import sys

PINNED = "6bc7c26cf67a9480b5ef9d6191aebe87fa931183"
HERE = os.path.dirname(os.path.abspath(__file__))
KNOWLEDGE = os.path.join(HERE, "..", "..", "..", "crates", "steins-catalog", "src", "knowledge.rs")

# ---------------------------------------------------------------- raise primitives

# Functions php-src raises an exception through. Each maps to a *kind*:
#   arg    -- raised for the type or count of an argument;
#   value  -- raised for a value, or for a state: needs a disposition.
PRIMITIVES = {
    "zend_throw_error": "value",
    "zend_throw_exception": "value",
    "zend_throw_exception_ex": "value",
    "zend_throw_exception_object": "value",
    "zend_throw_error_exception": "value",
    "zend_value_error": "value",
    "zend_argument_value_error": "value",
    "zend_argument_error": "value",
    "zend_argument_error_variadic": "value",
    "zend_type_error": "value",
    "zend_argument_type_error": "arg",
    "zend_argument_count_error": "arg",
    "zend_wrong_parameters_none_error": "arg",
    "zend_wrong_parameters_count_error": "arg",
    "zend_wrong_parameter_error": "arg",
    "zend_wrong_parameter_type_error": "arg",
    "zend_wrong_parameter_class_error": "arg",
    "zend_wrong_parameter_class_or_null_error": "arg",
    "zend_wrong_parameter_class_or_long_error": "arg",
    "zend_wrong_parameter_class_or_long_or_null_error": "arg",
    "zend_wrong_parameter_class_or_string_error": "arg",
    "zend_wrong_parameter_class_or_string_or_null_error": "arg",
    "zend_wrong_callback_error": "arg",
    "zend_wrong_callback_or_null_error": "arg",
    "zend_wrong_param_count": "arg",
    "zend_unexpected_extra_named_error": "arg",
    "zend_argument_count_error_ex": "arg",
}

# Macros that raise through the parameter parser: argument checking by
# construction (`ZEND_PARSE_PARAMETERS_END` ends in `zend_wrong_parameter_error`).
ARG_MACROS = re.compile(r"\b(ZEND_PARSE_PARAMETERS_(?:START|END|NONE)|Z_PARAM_\w+|"
                        r"zend_parse_parameters(?:_none)?|zend_parse_arg_\w+)\b")

# Calls the walk does not enter, tagged for the note instead. Each one hands
# control to something the audit does not read: user code, the class lookup
# (an autoloader), a conversion that can run `__toString`, or a release that can
# run a destructor.
BOUNDARIES = [
    ("zpp", re.compile(r"^(zend_parse_\w+|zend_wrong_\w+|zpp_\w+)$")),
    ("user-code", re.compile(r"^(zend_call_function|zend_call_known_\w+|zend_call_method\w*|"
                             r"call_user_function\w*|zend_fcall_info_call|_call_user_function_impl|"
                             r"zend_call_\w+)$")),
    ("autoload", re.compile(r"^(zend_lookup_class\w*|zend_fetch_class\w*|zend_class_lookup\w*)$")),
    ("to-string", re.compile(r"^(zval_get_string\w*|zval_try_get_string\w*|_zval_get_string_func|"
                             r"zval_try_get_tmp_string|zval_get_tmp_string|"
                             r"zend_parse_arg_str_weak|zend_parse_arg_str_slow)$")),
    # Property access on an object operand: a lazy object's initializer, a hook,
    # `__get` and `__toString` run user code, and an operand that can hold an
    # object is the `Object`/`Coerced`/`Nested` reach rows' (`arg_reach`) to
    # charge. Instantiating a class (`object_init_ex`) raises only for a class
    # that is abstract, an interface or an enum, and these callers name theirs.
    ("object-handlers", re.compile(r"^(zend_std_\w+|zend_get_properties_for|zend_lazy_object_\w+|"
                                   r"zend_get_property_info|zend_check_property_access|"
                                   r"zend_bad_property_\w+|object_init_ex|_object_and_properties_init|"
                                   r"object_properties_init\w*|zend_update_class_constants|"
                                   r"zend_hooked_object_\w+|zend_read_property\w*|zend_update_property\w*|"
                                   r"zend_enum_\w+|zend_verify_\w+|i_zend_verify_\w+)$")),
    # Warnings and notices: a user error handler can throw from one, which is
    # the error-handler lane (`set_error_handler`), not the builtin's own raise.
    ("diagnostic", re.compile(r"^(php_error_docref\w*|php_error\w*|zend_error\w*|php_verror|"
                              r"zend_throw_exception_internal|php_log_err\w*)$")),
    ("destructor", re.compile(r"^(zval_ptr_dtor\w*|zend_array_destroy|zend_hash_destroy|"
                              r"zend_hash_clean|zend_hash_graceful\w*|zend_hash_del\w*|"
                              r"zend_hash_index_del\w*|zend_hash_str_del\w*|_zend_hash_del\w*|"
                              r"zend_object_release|zend_objects_store_del|rc_dtor_func|"
                              r"zval_dtor\w*|zend_hash_apply\w*)$")),
]

MACRO_DEF = re.compile(r"^#define[ \t]+(\w+)\(([^)]*)\)((?:[^\n]*\\\n)*[^\n]*)", re.M)

C_KEYWORDS = {
    "if", "for", "while", "switch", "return", "sizeof", "do", "else", "case", "defined",
    "typeof", "__typeof__", "alignof", "_Alignof", "offsetof",
}

SKIP_DIRS = {"tests", "test", "benchmarks", "win32", "build", "docs", "docs-old", "scripts"}
MAX_DEPTH = 30


# ---------------------------------------------------------------- C reading

def mask(text):
    """`text` with comments, string and character literal contents replaced by
    spaces (newlines kept), so offsets and line numbers still match the source."""
    out = list(text)
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        two = text[i:i + 2]
        if two == "//":
            j = text.find("\n", i)
            j = n if j < 0 else j
            for k in range(i, j):
                out[k] = " "
            i = j
        elif two == "/*":
            j = text.find("*/", i + 2)
            j = n if j < 0 else j + 2
            for k in range(i, j):
                if text[k] != "\n":
                    out[k] = " "
            i = j
        elif c == '"' or c == "'":
            j = i + 1
            while j < n and text[j] != c:
                if text[j] == "\\":
                    j += 1
                if j < n and text[j] == "\n":
                    break
                j += 1
            for k in range(i + 1, min(j, n)):
                if text[k] != "\n":
                    out[k] = " "
            i = j + 1
        else:
            i += 1
    return "".join(out)


DEF_RE = re.compile(
    r"(?m)^(?![ \t#}])(?P<decl>[^;{}()\n]*?\b)?(?P<name>[A-Za-z_]\w*)[ \t]*"
    r"\((?P<args>[^;{}]*?)\)[ \t\r\n]*\{")


def match_brace(masked, open_at):
    depth = 0
    for i in range(open_at, len(masked)):
        c = masked[i]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return i
    return None


class Func:
    __slots__ = ("symbol", "path", "line", "body", "body_line", "static", "raw", "masked_body")

    def __init__(self, symbol, path, line, raw, masked_body, body_line, static):
        self.symbol, self.path, self.line = symbol, path, line
        self.raw, self.masked_body, self.body_line, self.static = raw, masked_body, body_line, static


class Tree:
    def __init__(self, root):
        self.root = root
        self.defs = {}        # symbol -> [Func]
        self.arginfo = {}     # php name -> (impl symbol, arginfo file)
        self.files = 0

    def scan(self):
        for base in ("Zend", "ext", "main", "sapi"):
            for dirpath, dirnames, filenames in os.walk(os.path.join(self.root, base)):
                dirnames[:] = sorted(d for d in dirnames if d not in SKIP_DIRS)
                for fn in sorted(filenames):
                    if fn.endswith("_arginfo.h"):
                        self.read_arginfo(os.path.join(dirpath, fn))
                    elif fn.endswith((".c", ".h", ".inc")):
                        self.read_c(os.path.join(dirpath, fn))

    def rel(self, path):
        return os.path.relpath(path, self.root)

    def read_arginfo(self, path):
        """The function tables only: `static const zend_function_entry
        ext_functions[]` and its siblings, never a `class_*_methods[]` table, whose
        entries are methods that merely share a spelling (`Collator::asort`)."""
        text = open(path, encoding="utf-8", errors="replace").read()
        for table in re.finditer(r"static const zend_function_entry (\w+)\[\] = \{(.*?)\n\};", text, re.S):
            if table.group(1).endswith("_methods"):
                continue
            body = table.group(2)
            for m in re.finditer(r"^\s*ZEND_FE\((\w+),", body, re.M):
                self.arginfo.setdefault(m.group(1).lower(), ("zif_" + m.group(1), self.rel(path)))
            for m in re.finditer(r'^\s*ZEND_RAW_FENTRY\("([^"]+)",\s*(\w+),', body, re.M):
                self.arginfo.setdefault(m.group(1).lower(), (m.group(2), self.rel(path)))

    def read_c(self, path):
        raw = open(path, encoding="utf-8", errors="replace").read()
        self.files += 1
        masked = mask(raw)
        rel = self.rel(path)
        for m in DEF_RE.finditer(masked):
            name, args, decl = m.group("name"), m.group("args"), m.group("decl") or ""
            if name in C_KEYWORDS:
                continue
            open_at = m.end() - 1
            symbol, static = name, "static" in decl.split()
            if name in ("PHP_FUNCTION", "ZEND_FUNCTION"):
                symbol = "zif_" + args.strip()
            elif name in ("PHP_NAMED_FUNCTION", "ZEND_NAMED_FUNCTION"):
                symbol = args.strip()
            elif name == "FileFunction":
                continue
            close = match_brace(masked, open_at)
            if close is None:
                continue
            line = raw.count("\n", 0, m.start("name")) + 1
            body_line = raw.count("\n", 0, open_at) + 1
            self.defs.setdefault(symbol, []).append(
                Func(symbol, rel, line, raw[open_at:close + 1], masked[open_at:close + 1],
                     body_line, static))
        if rel == "ext/standard/filestat.c":
            self.read_file_functions(raw, masked, rel)
        self.read_macro_functions(raw, rel)

    def read_file_functions(self, raw, masked, rel):
        """`FileFunction(PHP_FN(name), FS_X)` instances: one macro body, many names."""
        macro = re.search(r"#define FileFunction\(name, funcnum\)([^\n]*\\\n)+[^\n]*", raw)
        if not macro:
            return
        body_text = macro.group(0)
        body_line = raw.count("\n", 0, macro.start()) + 1
        for m in re.finditer(r"^FileFunction\(PHP_FN\((\w+)\),\s*(FS_\w+)\)", raw, re.M):
            sym = "zif_" + m.group(1)
            text = body_text.replace("funcnum", m.group(2)).replace("\\\n", "\n")
            line = raw.count("\n", 0, m.start()) + 1
            self.defs.setdefault(sym, []).append(
                Func(sym, rel, line, text, mask(text), body_line, False))

    def read_macro_functions(self, raw, rel):
        """Functions a `##`-pasting macro defines when invoked at file scope:
        `DEFINE_SORT_VARIANTS(data_compare)` in ext/standard/array.c is what makes
        `php_array_data_compare_unstable`, the comparator `sort()` runs, exist."""
        for macro in MACRO_DEF.finditer(raw):
            name, params, body = macro.group(1), macro.group(2), macro.group(3)
            if "##" not in body or "{" not in body:
                continue
            names = [x.strip() for x in params.split(",") if x.strip()]
            for call in re.finditer(r"^%s\(([^)\n]*)\);?[ \t]*$" % re.escape(name), raw, re.M):
                args = [x.strip() for x in call.group(1).split(",")]
                if len(args) != len(names):
                    continue
                text = body.replace("\\\n", "\n")
                for param, arg in zip(names, args):
                    text = re.sub(r"\b%s\b" % re.escape(param), arg, text)
                text = re.sub(r"[ \t]*##[ \t]*", "", text)
                text = "\n".join(part.lstrip() for part in text.split("\n"))
                line = raw.count("\n", 0, call.start()) + 1
                expanded = mask(text)
                for m in DEF_RE.finditer(expanded):
                    fname = m.group("name")
                    if fname in C_KEYWORDS:
                        continue
                    open_at = m.end() - 1
                    close = match_brace(expanded, open_at)
                    if close is None:
                        continue
                    self.defs.setdefault(fname, []).append(
                        Func(fname, rel, line, text[open_at:close + 1], expanded[open_at:close + 1],
                             line, "static" in (m.group("decl") or "").split()))

    def lookup(self, symbol, from_path):
        found = self.defs.get(symbol, [])
        if not found:
            return []
        same = [f for f in found if f.path == from_path]
        if same:
            return same
        public = [f for f in found if not f.static]
        return public or found


# ---------------------------------------------------------------- the walk

CALL_RE = re.compile(r"\b([A-Za-z_]\w*)\s*\(")
IDENT_RE = re.compile(r"\b[A-Za-z_]\w*\b")


def first_args(raw_body, masked_body, at):
    """The raw text of the call's argument list starting at the `(` after `at`."""
    open_at = masked_body.index("(", at)
    depth = 0
    for i in range(open_at, len(masked_body)):
        if masked_body[i] == "(":
            depth += 1
        elif masked_body[i] == ")":
            depth -= 1
            if depth == 0:
                return raw_body[open_at + 1:i]
    return raw_body[open_at + 1:]


def split_top(args):
    parts, depth, cur, instr = [], 0, [], None
    for c in args:
        if instr:
            cur.append(c)
            if c == instr:
                instr = None
            continue
        if c in "\"'":
            instr = c
            cur.append(c)
        elif c in "([{":
            depth += 1
            cur.append(c)
        elif c in ")]}":
            depth -= 1
            cur.append(c)
        elif c == "," and depth == 0:
            parts.append("".join(cur).strip())
            cur = []
        else:
            cur.append(c)
    parts.append("".join(cur).strip())
    return parts


def string_literals(text):
    return "".join(re.findall(r'"((?:[^"\\]|\\.)*)"', text))


class Raise:
    def __init__(self, primitive, func, line, cls, message, chain):
        self.primitive, self.func, self.line, self.cls, self.message = primitive, func, line, cls, message
        self.chain = chain

    def key(self):
        return (self.primitive, self.func.path, self.line)


def exception_class(prim, args):
    parts = split_top(args)
    if prim in ("zend_throw_error", "zend_throw_exception", "zend_throw_exception_ex"):
        c = parts[0] if parts else ""
        if c in ("NULL", "0", ""):
            return "Error"
        m = re.match(r"zend_ce_(\w+)$", c)
        if m:
            return {"value_error": "ValueError", "type_error": "TypeError",
                    "argument_count_error": "ArgumentCountError",
                    "arithmetic_error": "ArithmeticError",
                    "division_by_zero_error": "DivisionByZeroError",
                    "exception": "Exception", "error_exception": "ErrorException",
                    "unhandled_match_error": "UnhandledMatchError",
                    "error": "Error"}.get(m.group(1), c)
        return c
    return {"zend_value_error": "ValueError", "zend_argument_value_error": "ValueError",
            "zend_type_error": "TypeError", "zend_argument_type_error": "TypeError",
            "zend_argument_count_error": "ArgumentCountError"}.get(prim, prim)


def walk(tree, root_func):
    """Raise primitives and boundary tags reachable from `root_func`."""
    raises, tags = {}, {}
    seen = set()
    stack = [(root_func, [root_func.symbol], 0)]
    while stack:
        func, chain, depth = stack.pop()
        ident = (func.symbol, func.path, func.line)
        if ident in seen:
            continue
        seen.add(ident)
        body, raw = func.masked_body, func.raw
        arg_macro = ARG_MACROS.search(body)
        if arg_macro:
            tags.setdefault("zpp", (func.path, func.body_line + body.count("\n", 0, arg_macro.start())))
        for m in CALL_RE.finditer(body):
            name = m.group(1)
            if name in C_KEYWORDS:
                continue
            line = func.body_line + body.count("\n", 0, m.start())
            if name in PRIMITIVES:
                if body[:m.start()].rstrip().endswith("ZEND_UNREACHABLE();"):
                    continue  # raised only on a path the engine marks unreachable
                args = first_args(raw, body, m.start())
                parts = split_top(args)
                msg_text = string_literals(args) if name != "zend_throw_exception_object" else ""
                cls = exception_class(name, args)
                r = Raise(name, func, line, cls, " ".join(msg_text.split()), chain)
                raises.setdefault(r.key(), r)
                continue
            boundary = next((tag for tag, rx in BOUNDARIES if rx.match(name)), None)
            if boundary:
                tags.setdefault(boundary, (func.path, line))
                continue
            if depth >= MAX_DEPTH:
                continue
            for callee in tree.lookup(name, func.path):
                stack.append((callee, chain + [name], depth + 1))
        # A function named without being called (`zend_sort(..., php_array_data_compare)`,
        # a table of handlers) is reached through its address: an edge as well.
        # Only to a function of the same file: the comparators and handlers a
        # builtin passes along are its own file's, and following an address
        # anywhere in the tree would reach every function a local variable's
        # name happens to spell.
        if depth < MAX_DEPTH:
            for m in IDENT_RE.finditer(body):
                name = m.group(0)
                if name in tree.defs and not body[m.end():].lstrip().startswith("("):
                    for callee in tree.defs[name]:
                        if callee.path == func.path:
                            stack.append((callee, chain + ["&" + name], depth + 1))
    return list(raises.values()), tags


# ---------------------------------------------------------------- dispositions

# What the audit recorded for each name with a non-argument raise primitive:
# (class, reason). A name absent from this table with such a primitive is
# `unreviewed`. A disposition is a claim about the pinned php-src; a re-pin
# re-reads it against the regenerated note. Each reason says which raise sites
# it covers and why none of them is raised for an admitted argument value.
_TZDB = ("none", "date_ce_date_error 'Timezone database is corrupt' (php_date.c:572) is raised when "
                 "the zone database lacks the guessed default zone: environment, no argument selects "
                 "it (an invalid `date.timezone` falls back to UTC, `date_default_timezone_set` "
                 "refuses an unknown id)")
_RANDOM = ("none", "RandomException (csprng.c:217) needs the operating system's CSPRNG to fail while "
                   "seeding: environment. BrokenRandomEngineError (engine_user.c:59, random.c:114, "
                   "random.c:173) needs a `Random\\Engine` that returns nothing or a constant, and the "
                   "global MT engine these functions use is neither")
_STREAM = ("none", "8.6, registration: the stream error machinery (stream_errors.c, new in 8.6; no "
                   "`StreamException` on 8.5.11) throws when the stream's *context options* "
                   "`error_mode`/`error_handler` ask it to, as a user stream wrapper's exceptions do, "
                   "so it is attributed to the call that registers the context (ADR-0099 §4.5), not "
                   "to an argument of this call. A closed handle is the argument-checking TypeError")
_RECURSIVE_COMPARE = ("needs-row", "Error 'Nesting level too deep - recursive dependency?' "
                      "(zend_hash.c:3229) when two distinct arrays that contain themselves by "
                      "reference are compared with `==`, `===` or `<=>` (witnessed on 8.5.11)")
DISPOSITIONS = {
    "array_diff": ("argument-checking", "TypeErrors are argument checks; array.c:5852 needs a total of "
                   "HT_MAX_SIZE (2^30) elements held at once: capacity, no admitted value"),
    "array_filter": ("none", "8.6: ValueError 'must be one of ARRAY_FILTER_USE_*' (array.c:6534) for an "
                     "`int $mode` outside 0..=2 is new in 8.6.0-dev (UPGRADING: array_filter) and a "
                     "silent no-op on 8.5.11, the line the catalog is pinned to (`PINNED_PHP`)"),
    "array_keys": _RECURSIVE_COMPARE,
    "array_merge": ("argument-checking", "array.c:4185 is an argument check; array.c:4192 needs 2^30 "
                    "elements (capacity); array.c:3950 and zend_execute.c:2595 are on the "
                    "`array_merge_recursive` path of the shared wrapper"),
    "array_replace": ("argument-checking", "array.c:4081 is on the `array_replace_recursive` path of the "
                      "shared wrapper (witnessed: `array_replace($r, $r2)` of two recursive arrays "
                      "returns)"),
    "array_replace_recursive": ("needs-row", "Error 'Recursion detected' (array.c:4081) for an array that "
                                "contains itself by reference (witnessed on 8.5.11)"),
    "array_search": _RECURSIVE_COMPARE,
    "array_splice": ("destructor-hazard", "Error 'Array was modified during array_splice operation' "
                     "(array.c:3384) when a destructor of a removed element drops the last reference "
                     "to the array being spliced (ADR-0099 §7.1): needs user code inside the call"),
    "array_unique": ("needs-row", "Error 'Nesting level too deep - recursive dependency?' "
                     "(zend_hash.c:3229) under `SORT_REGULAR`, which compares the elements with `==`"),
    "array_walk": ("none", "array.c:1454 is on the `array_walk_recursive` path of the shared walker; "
                   "array.c:1490 needs the callback to replace the array being walked, and the "
                   "callback is a `Callback` reach that already holds the gap"),
    "array_walk_recursive": ("needs-row", "Error 'Recursion detected' (array.c:1454) for an array that "
                             "contains itself by reference (witnessed on 8.5.11); array.c:1490 needs "
                             "the callback to replace the array"),
    "arsort": _RECURSIVE_COMPARE,
    "asort": _RECURSIVE_COMPARE,
    "date": _TZDB,
    "date_create": ("needs-row", "Error 'The DateTimeZone object has not been correctly initialized by "
                    "its constructor' (php_date.c:2427) for a user subclass of `DateTimeZone` whose "
                    "constructor does not call the parent's (witnessed on 8.5.11); php_date.c:2401 is "
                    "the constructor's (PHP_DATE_INIT_CTOR), and the function returns false"),
    "date_create_immutable": ("needs-row", "as `date_create`: an uninitialised `DateTimeZone` subclass "
                              "is an Error (php_date.c:2427)"),
    "date_default_timezone_get": _TZDB,
    "fclose": _STREAM,
    "feof": _STREAM,
    "fflush": _STREAM,
    "fputs": _STREAM,
    "fseek": _STREAM,
    "ftell": _STREAM,
    "fwrite": _STREAM,
    "getdate": _TZDB,
    "gmdate": _TZDB,
    "gmmktime": _TZDB,
    "idate": _TZDB,
    "implode": ("argument-checking", "string.c:1028 and string.c:1040 are TypeErrors for the type of "
                "`$separator` or `$array`"),
    "in_array": _RECURSIVE_COMPARE,
    "join": ("argument-checking", "alias of `implode`"),
    "krsort": ("none", "compares keys, which are `int|string` zvals: never an array"),
    "ksort": ("none", "compares keys, which are `int|string` zvals: never an array"),
    "lcg_value": _RANDOM,
    "localtime": _TZDB,
    "microtime": _TZDB,
    "mktime": _TZDB,
    "mt_srand": _RANDOM,
    "pathinfo": ("none", "8.6: ValueError 'must be one of / only one of the PATHINFO_* constants' "
                 "(string.c:1568, 1573) for an `int $flags` that is not one of the four flags or "
                 "`PATHINFO_ALL` is new in 8.6.0-dev (UPGRADING: pathinfo); 8.5.11 accepts it"),
    "rand": _RANDOM,
    "rewind": _STREAM,
    "rsort": _RECURSIVE_COMPARE,
    "shuffle": _RANDOM,
    "sort": _RECURSIVE_COMPARE,
    "srand": _RANDOM,
    "stream_context_get_options": ("argument-checking", "zend_list.c fetch errors and "
                                   "streamsfuncs.c:1034 are TypeErrors for a resource that is not an "
                                   "open stream or context"),
    "stream_get_meta_data": _STREAM,
    "strtotime": _TZDB,
    "tmpfile": _STREAM,
    "uniqid": _RANDOM,
}


def load_names():
    text = open(KNOWLEDGE, encoding="utf-8").read()
    block = re.search(r"const THROWLESS_NAMES: &\[&str\] = &\[(.*?)\];", text, re.S).group(1)
    return re.findall(r'"([a-z0-9_]+)"', block)


def classify(name, raises):
    non_arg = [r for r in raises if PRIMITIVES[r.primitive] != "arg"]
    if name in DISPOSITIONS:
        if not non_arg:
            sys.exit(f"stale disposition: {name} reaches no non-argument raise at this pin")
        return DISPOSITIONS[name]
    if not raises:
        return "none", ""
    if not non_arg:
        return "argument-checking", ""
    return "unreviewed", ""


class Row:
    def __init__(self, name, on_table, sym, func, raises, tags, cls, why):
        self.name, self.on_table, self.sym, self.func = name, on_table, sym, func
        self.raises, self.tags, self.cls, self.why = raises, tags, cls, why


def pinned_head(php_src):
    return subprocess.run(["git", "-C", php_src, "rev-parse", "HEAD"], capture_output=True,
                          text=True).stdout.strip()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--php-src", default=os.environ.get("PHP_SRC_ROOT",
                                                        os.path.expanduser("~/local/src/php-src")))
    ap.add_argument("--out")
    ap.add_argument("--allow-other-pin", action="store_true")
    a = ap.parse_args()
    head = pinned_head(a.php_src)
    if head != PINNED and not a.allow_other_pin:
        sys.exit(f"php-src is at {head}, not the pin {PINNED}")
    tree = Tree(a.php_src)
    tree.scan()
    on_table = set(load_names())
    # A name the audit took off the table keeps its row in the note, so a later
    # edit that puts it back meets the reason.
    names = sorted(on_table | {n for n, (c, _) in DISPOSITIONS.items() if c == "needs-row"})
    rows = []
    for name in names:
        impl = tree.arginfo.get(name)
        if impl is None:
            sys.exit(f"{name}: no arginfo entry at this pin")
        sym, _ = impl
        defs = tree.defs.get(sym, [])
        if not defs:
            sys.exit(f"{name}: no definition of {sym} at this pin")
        func = defs[0]
        raises, tags = walk(tree, func)
        cls, why = classify(name, raises)
        rows.append(Row(name, name in on_table, sym, func, raises, tags, cls, why))
    emit(rows, a)


def emit(rows, a):
    out = []
    w = out.append
    w("<!-- Generated by docs/research/phpsrc-mining/audit_throwless.py; do not edit. -->")
    w("# Throwless table audit")
    w("")
    w(f"php-src commit: `{PINNED}` (Thu Jul 9 2026, PHP 8.6.0-dev)")
    w("")
    w("Each name on `THROWLESS_NAMES` (`crates/steins-catalog/src/knowledge.rs`), resolved through the "
      "generated `*_arginfo.h` entry tables to the C function that runs it, with the raise calls "
      "(`zend_throw_error`, `zend_value_error`, `zend_argument_value_error`, `zend_throw_exception*`, "
      "...) its call graph reaches. The classes:")
    w("")
    w("* `none`: no raise is reached for any admitted argument value of the catalog's PHP line "
      "(`PINNED_PHP`, 8.5). The pin is php-src master, 8.6.0-dev, and a reason that starts `8.6:` is "
      "a raise that exists there and not on 8.5.11 (each was probed on 8.5.11).")
    w("* `argument-checking`: what is raised depends on the type or count of an argument, or on a "
      "resource's validity, which the throw lane sets aside for every builtin (ADR-0099 §3.3).")
    w("* `destructor-hazard`: the raise needs a destructor to have run inside the call (ADR-0099 §7.1).")
    w("* `needs-row`: a call raises for an argument value its parameter types admit. Such a name is "
      "not on the table and has a `builtin_throws` row.")
    w("")
    w("The walk does not enter the engine's parameter parsing (`zpp`), user code (`user-code`: "
      "callbacks and `zend_call_*`), the class lookup (`autoload`), conversions that run `__toString` "
      "(`to-string`), object property handlers, lazy initializers and hooks (`object-handlers`), "
      "diagnostics a user error handler can turn into an exception (`diagnostic`), or releases that "
      "can run a destructor (`destructor`). What an operand can run there is the reach rows' "
      "(`arg_reach`) to charge, not the throw table's. The column `reached` lists the ones a name's "
      "walk met.")
    w("")
    counts = {}
    for r in rows:
        counts[r.cls] = counts.get(r.cls, 0) + 1
    w("Classes: " + ", ".join(f"{k} {v}" for k, v in sorted(counts.items())) + f" ({len(rows)} names).")
    w("")
    w("| name | table | c function | class | reached |")
    w("| --- | --- | --- | --- | --- |")
    for r in rows:
        where = f"{r.func.path}:{r.func.line}"
        reached = ", ".join(sorted(r.tags))
        w(f"| `{r.name}` | {'yes' if r.on_table else 'no'} | `{r.sym}` {where} | {r.cls} | {reached} |")
    w("")
    w("## Raise sites and dispositions")
    w("")
    w("Per name: the number of argument-checking raises its walk reaches, every other raise site "
      "(exception class, file:line, the C function that raises it, the message) and the reason the "
      "class holds. The walk is path-insensitive, so a site can be one the name never takes; the "
      "reason says so when it is.")
    w("")
    for r in rows:
        arg = [x for x in r.raises if PRIMITIVES[x.primitive] == "arg"]
        other = [x for x in r.raises if PRIMITIVES[x.primitive] != "arg"]
        if not other and not arg:
            continue
        w(f"### `{r.name}` ({r.cls})")
        w("")
        if arg:
            w(f"* {len(arg)} argument-checking raise site{'s' if len(arg) != 1 else ''}")
        for x in sorted(other, key=lambda x: (x.func.path, x.line)):
            msg = f": {x.message}" if x.message else ""
            w(f"* {x.cls} `{x.func.path}:{x.line}` in `{x.func.symbol}`{msg}")
        if r.why:
            w(f"* reason: {r.why}")
        w("")
    text = "\n".join(out).rstrip("\n") + "\n"
    if a.out:
        open(a.out, "w", encoding="utf-8").write(text)
    else:
        sys.stdout.write(text)


if __name__ == "__main__":
    main()
