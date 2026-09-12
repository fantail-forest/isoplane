#!/usr/bin/env python3
"""Check managed LibraryImport declarations into the four native shims against
their C prototypes, as wasm signatures.

    guest-runtime/audit-pinvokes.py <runtime tree>

A native ABI ignores a mismatched return type or a widened integer; wasm
traps on the call instead, so a declaration that merely disagrees with its
C definition is a guest crash waiting for the first caller. Exits 1 and
lists the mismatches, or 0 with the count of declarations checked.
"""
import glob
import os
import re
import sys

SHIMS = {
    "SystemNative": "System.Native",
    "CryptoNative": "System.Security.Cryptography.Native",
    "GlobalizationNative": "System.Globalization.Native",
    "CompressionNative": "System.IO.Compression.Native",
}
I64_C = {"int64_t", "uint64_t", "long long", "unsigned long long", "off_t", "int64", "uint64"}
I64_CS = {"long", "ulong", "Int64", "UInt64"}


def strip_comments(text):
    text = re.sub(r"/\*.*?\*/", " ", text, flags=re.S)
    return re.sub(r"//[^\n]*", " ", text)


def c_wasm_type(ctype):
    t = re.sub(r"\b(const|volatile|struct|enum)\b", " ", ctype)
    t = re.sub(r"\s+", " ", t).strip()
    if t == "void":
        return "void"
    if "*" in t or "(" in t:
        return "i32"
    if t in I64_C:
        return "i64"
    if t == "double":
        return "f64"
    if t == "float":
        return "f32"
    return "i32"


def c_param_type(param):
    param = param.strip()
    if param in ("", "void"):
        return None
    param = re.sub(r"\[[^\]]*\]$", "*", param)
    if "(*" in param:
        return "i32"
    match = re.match(r"^(.*?)\s*\b[A-Za-z_]\w*$", param)
    if match and not param.endswith("*"):
        param = match.group(1)
    return c_wasm_type(param)


def native_prototypes(tree):
    prototypes = {}
    headers = []
    for shim in SHIMS.values():
        headers.extend(glob.glob(os.path.join(tree, "src", "native", "libs", shim, "*.h")))
    headers.extend(glob.glob(os.path.join(tree, "src", "native", "external", "brotli", "c", "include", "brotli", "*.h")))
    for header in headers:
        text = strip_comments(open(header).read())
        text = re.sub(r"\bFUNCTIONCALLINGCONVENTION\b", "", text)
        for match in re.finditer(r"(?:PALEXPORT|FUNCTIONEXPORT|BROTLI_(?:DEC|ENC|COMMON)_API)\s+(?P<ret>[\w\s\*()]+?)\s*\b(?P<name>\w+)\s*\((?P<args>[^;]*?)\)\s*;", text, re.S):
            args = [c_param_type(a) for a in split_params(match.group("args"))]
            prototypes[match.group("name")] = (c_wasm_type(match.group("ret")), [a for a in args if a])
    return prototypes


def i64_enums(text):
    return set(re.findall(r"\benum\s+(\w+)\s*:\s*(?:long|ulong|Int64|UInt64)\b", text))


def cs_wasm_type(cstype, wide_enums):
    t = re.sub(r"\[[^\]]*\]", " ", cstype)
    t = re.sub(r"\b(scoped|readonly|params)\b", " ", t)
    t = re.sub(r"\s+", " ", t).strip()
    if re.match(r"^(ref|out|in)\b", t):
        return "i32"
    if t == "void":
        return "void"
    if "*" in t or t.endswith("[]") or "Span<" in t:
        return "i32"
    if t in I64_CS or t.split(".")[-1] in wide_enums:
        return "i64"
    if t == "double":
        return "f64"
    if t in ("float", "Single"):
        return "f32"
    return "i32"


def cs_param_type(param, wide_enums):
    param = param.strip()
    if not param:
        return None
    param = re.sub(r"=.*$", "", param)
    match = re.match(r"^(.*?)\s*\b@?[A-Za-z_]\w*$", param, re.S)
    return cs_wasm_type(match.group(1) if match else param, wide_enums)


def managed_declarations(tree):
    declarations = []
    wide_enums = set()
    sources = glob.glob(os.path.join(tree, "src", "libraries", "**", "*.cs"), recursive=True)
    texts = {}
    for source in sources:
        if os.sep + "tests" + os.sep in source or os.sep + "ref" + os.sep in source:
            continue
        text = open(source, encoding="utf-8", errors="replace").read()
        if "LibraryImport(Libraries." not in text:
            continue
        texts[source] = strip_comments(text)
        wide_enums |= i64_enums(texts[source])
    pattern = re.compile(
        r"\[LibraryImport\(Libraries\.(?P<lib>\w+)(?P<attr>[^\]]*)\](?P<decl>.*?);", re.S
    )
    for source, text in texts.items():
        for match in pattern.finditer(text):
            if match.group("lib") not in SHIMS:
                continue
            entry = re.search(r'EntryPoint\s*=\s*"(\w+)"', match.group("attr"))
            decl = match.group("decl")
            signature = re.search(
                r"partial\s+(?P<ret>(?:\[[^\]]*\]\s*)*[\w\.<>?\*\[\]]+(?:\s*\*)*)\s+(?P<name>\w+)\s*\((?P<args>.*)\)\s*$",
                decl, re.S,
            )
            if signature is None:
                raise ValueError(f"{os.path.relpath(source, tree)}: cannot parse selected LibraryImport declaration")
            name = entry.group(1) if entry else signature.group("name")
            args = [cs_param_type(a, wide_enums) for a in split_params(signature.group("args"))]
            declarations.append((
                name,
                cs_wasm_type(signature.group("ret"), wide_enums),
                [a for a in args if a],
                os.path.relpath(source, tree),
            ))
    return declarations


def split_params(text):
    depth = 0
    current = []
    for char in text:
        if char in "<([":
            depth += 1
        elif char in ">)]":
            depth -= 1
        if char == "," and depth == 0:
            yield "".join(current)
            current = []
        else:
            current.append(char)
    if current:
        yield "".join(current)


def main():
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    tree = sys.argv[1]
    native = native_prototypes(tree)
    try:
        managed = managed_declarations(tree)
    except ValueError as error:
        print(error, file=sys.stderr)
        return 1
    if not native or not managed:
        print("no native prototypes or managed declarations found; verify the runtime source path", file=sys.stderr)
        return 1
    checked = 0
    mismatches = []
    for name, ret, args, source in sorted(managed):
        if name not in native:
            mismatches.append(f"{name} ({source}): no parsed native prototype")
            continue
        checked += 1
        native_ret, native_args = native[name]
        if (ret, args) != (native_ret, native_args):
            mismatches.append(f"{name} ({source}): managed {ret} {args}, native {native_ret} {native_args}")
    for line in mismatches:
        print(line)
    if mismatches:
        print(f"{len(mismatches)} P/Invoke declarations disagree with their C prototypes (a wasm trap on call)", file=sys.stderr)
        return 1
    print(f"{checked} P/Invoke declarations match their C prototypes")
    return 0


if __name__ == "__main__":
    sys.exit(main())
