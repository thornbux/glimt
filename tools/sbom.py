#!/usr/bin/env python3
"""Write sbom.cdx.json, a CycloneDX 1.5 SBOM of Glimt and the crates it is built from (Linux,
all features), from `cargo metadata` and the checksums in Cargo.lock, and THIRD-PARTY-LICENSES.txt,
the license texts of those crates, which their licenses ask to ship with the program. Run it from
the repository root after changing dependencies: python3 tools/sbom.py"""

import json
import os
import re
import subprocess
import tomllib
import uuid

meta = json.loads(subprocess.check_output([
    "cargo", "metadata", "--format-version", "1", "--locked", "--all-features",
    "--filter-platform", "x86_64-unknown-linux-gnu",
]))
with open("Cargo.lock", "rb") as f:
    checksums = {(p["name"], p["version"]): p.get("checksum") for p in tomllib.load(f)["package"]}

packages = {p["id"]: p for p in meta["packages"]}
nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
root = meta["resolve"]["root"]


def deps(node_id):
    """Dependencies used to build or run, not dev-dependencies (tests only)."""
    return sorted(
        d["pkg"] for d in nodes[node_id]["deps"]
        if any(k["kind"] != "dev" for k in d["dep_kinds"])
    )


# Only what the build actually reaches from Glimt.
used, todo = set(), [root]
while todo:
    node_id = todo.pop()
    if node_id not in used:
        used.add(node_id)
        todo.extend(deps(node_id))


def ref(node_id):
    p = packages[node_id]
    return f"pkg:cargo/{p['name']}@{p['version']}"


def component(node_id, kind="library"):
    p = packages[node_id]
    c = {"type": kind, "bom-ref": ref(node_id), "name": p["name"], "version": p["version"]}
    if p.get("description"):
        c["description"] = p["description"].strip()
    if p.get("license"):
        # Old crates write "MIT/Apache-2.0"; SPDX says "MIT OR Apache-2.0".
        c["licenses"] = [{"expression": p["license"].replace("/", " OR ")}]
    c["purl"] = ref(node_id)
    checksum = checksums.get((p["name"], p["version"]))
    if checksum:
        c["hashes"] = [{"alg": "SHA-256", "content": checksum}]
    if p.get("repository"):
        c["externalReferences"] = [{"type": "vcs", "url": p["repository"]}]
    return c


# The licenses compatible with Glimt's MIT license, which a crate may be under. A crate under
# another fails the script, so a new dependency's license is weighed before it is shipped.
# epaint's default fonts are under OFL-1.1 and the Ubuntu font license, which allow fonts to be
# bundled with a program under any license.
ALLOWED_LICENSES = {
    "MIT", "MIT-0", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "0BSD", "ISC", "Zlib",
    "Unicode-3.0", "Unicode-DFS-2016", "MPL-2.0", "CC0-1.0", "Unlicense", "BSL-1.0",
    "OFL-1.1", "Ubuntu-font-1.0",
}


def is_allowed(expression):
    """Whether the SPDX license expression `expression` lets the crate be used under the
    licenses of ALLOWED_LICENSES: one side of each OR, and both sides of each AND. An exception
    after WITH (LLVM-exception) only adds permissions, so it is read as the license before it.
    The words are turned into Python's True, False, and, or and brackets, and nothing else, so
    the text that is evaluated holds no word of the crate's own."""
    words = re.findall(r"\(|\)|[^\s()]+", expression)
    python, skip = [], False
    for word in words:
        if skip:
            skip = False
        elif word == "WITH":
            skip = True
        elif word in ("AND", "OR"):
            python.append(word.lower())
        elif word in ("(", ")"):
            python.append(word)
        else:
            python.append(str(word in ALLOWED_LICENSES))
    try:
        return eval(" ".join(python), {"__builtins__": {}}) is True
    except SyntaxError:
        return False


assert is_allowed("(MIT OR Apache-2.0) AND Unicode-3.0")
assert is_allowed("Apache-2.0 WITH LLVM-exception OR GPL-3.0-only")
assert not is_allowed("MIT AND GPL-3.0-only") and not is_allowed("GPL-3.0-or-later")
assert not is_allowed("MIT OR") and not is_allowed("")

not_allowed = [
    f"{packages[i]['name']} {packages[i]['version']}: {packages[i].get('license') or 'no license'}"
    for i in sorted(used - {root}, key=ref)
    if not is_allowed((packages[i].get("license") or "").replace("/", " OR "))
]
if not_allowed:
    raise SystemExit("Crates under a license not in ALLOWED_LICENSES:\n" + "\n".join(not_allowed))

sbom = {
    "bomFormat": "CycloneDX",
    "specVersion": "1.5",
    # Derived from the dependency set, so an unchanged tree gives an unchanged file: by name and
    # version, as cargo's ids hold the checkout's path, which differs on every machine.
    "serialNumber": "urn:uuid:" + str(uuid.uuid5(uuid.NAMESPACE_URL, " ".join(sorted(ref(u) for u in used)))),
    "version": 1,
    "metadata": {
        "tools": {"components": [{"type": "application", "name": "tools/sbom.py"}]},
        "component": component(root, "application"),
    },
    "components": [component(i) for i in sorted(used - {root}, key=ref)],
    "dependencies": [
        {"ref": ref(i), "dependsOn": [ref(d) for d in deps(i)]} for i in sorted(used, key=ref)
    ],
}
with open("sbom.cdx.json", "w") as f:
    json.dump(sbom, f, indent=1)
    f.write("\n")
print(f"sbom.cdx.json: {len(sbom['components'])} components")


def license_files(node_id):
    """The license texts a crate ships: LICENSE*, COPYING* and NOTICE* beside its Cargo.toml."""
    p = packages[node_id]
    folder = os.path.dirname(p["manifest_path"])
    names = {f for f in os.listdir(folder) if re.match(r"(licen[cs]e|copying|notice)", f, re.I)}
    if p.get("license_file"):
        names.add(p["license_file"])
    for name in sorted(names):
        path = os.path.join(folder, name)
        if os.path.isfile(path):
            with open(path, encoding="utf-8", errors="replace") as f:
                yield name, f.read().strip()


# Each text once, with the crates it covers: most share the same Apache text.
covers, bare = {}, []
for i in sorted(used - {root}, key=ref):
    p = packages[i]
    crate = f"{p['name']} {p['version']}"
    texts = list(license_files(i))
    for name, text in texts:
        covers.setdefault(text, []).append(f"{crate} ({name})")
    if not texts:
        bare.append(f"{crate}: {p.get('license', 'no license given')}")
with open("THIRD-PARTY-LICENSES.txt", "w") as f:
    f.write("Glimt is built from the crates below. Their license texts follow, each once, after the\n"
            "crates it covers. Glimt's own license is in LICENSE.\n")
    if bare:
        f.write("\nCrates that ship no license text, with the license they name:\n\n")
        f.write("".join(f"  {b}\n" for b in bare))
    for text, crates in covers.items():
        f.write("\n" + "=" * 78 + "\n")
        f.write("".join(f"{c}\n" for c in crates))
        f.write("-" * 78 + "\n\n" + text + "\n")
print(f"THIRD-PARTY-LICENSES.txt: {len(covers)} texts, {len(bare)} crates without one")
