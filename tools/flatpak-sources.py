#!/usr/bin/env python3
"""Write flatpak/cargo-sources.json, the crates of Cargo.lock as Flatpak sources, so the Flatpak
builds without the network (as Flathub builds): each crate from crates.io by the checksum in
Cargo.lock, unpacked where Cargo reads vendored crates, and a Cargo config that points there.
Run it from the repository root after changing dependencies: python3 tools/flatpak-sources.py"""

import json
import tomllib

with open("Cargo.lock", "rb") as f:
    lock = tomllib.load(f)

sources = []
for p in lock["package"]:
    if "source" not in p:
        continue  # Glimt itself
    if not p["source"].startswith("registry+"):
        raise SystemExit(f"{p['name']}: only crates.io crates can be vendored here")
    name, version, sha = p["name"], p["version"], p["checksum"]
    dest = f"cargo/vendor/{name}-{version}"
    sources.append({
        "type": "archive",
        "archive-type": "tar-gzip",
        "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
        "sha256": sha,
        "dest": dest,
    })
    # Cargo checks a vendored crate against this; no files listed, so none is checked one by one.
    sources.append({
        "type": "inline",
        "contents": json.dumps({"package": sha, "files": {}}),
        "dest": dest,
        "dest-filename": ".cargo-checksum.json",
    })
# In CARGO_HOME (the build's cargo folder); the path is from the folder that holds it.
sources.append({
    "type": "inline",
    "contents": '[source.vendored-sources]\ndirectory = "cargo/vendor"\n\n'
                '[source.crates-io]\nreplace-with = "vendored-sources"\n',
    "dest": "cargo",
    "dest-filename": "config.toml",
})
with open("flatpak/cargo-sources.json", "w") as f:
    json.dump(sources, f, indent=1)
    f.write("\n")
