#!/usr/bin/env python3
"""Write sbom.cdx.json, a CycloneDX 1.5 SBOM of Glimt and the crates it is built from (Linux,
all features), from `cargo metadata` and the checksums in Cargo.lock. Run it from the repository
root after changing dependencies: python3 tools/sbom.py"""

import json
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
