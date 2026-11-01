#!/usr/bin/env python3
"""Write the Flathub repository's files for a release into a folder: the manifest of
flatpak/io.github.thornbux.Glimt.yml with Glimt from its tag on GitHub instead of this checkout,
and cargo-sources.json. Flathub builds the files of its repository flathub/io.github.thornbux.Glimt,
so each release is a pull request there with these two files.
Run it from the repository root after the tag is pushed:
    python3 tools/flathub-manifest.py v0.50.0 ../flathub-glimt"""

import os
import shutil
import subprocess
import sys

if len(sys.argv) != 3:
    raise SystemExit(__doc__)
tag, out = sys.argv[1], sys.argv[2]
# The tag's commit, which Flathub checks the tag against: a tag moved later fails the build.
commit = subprocess.run(["git", "rev-list", "-n", "1", tag], capture_output=True, text=True,
                        check=True).stdout.strip()
manifest = "flatpak/io.github.thornbux.Glimt.yml"
with open(manifest) as f:
    text = f.read()
local = """      - type: dir
        path: ..
        skip:
          - target
          - .git
          - demo
          - build-flatpak
          - .flatpak-builder
"""
if local not in text:
    raise SystemExit(f"{manifest}: its source of this checkout has changed; change this tool too")
text = text.replace(local, f"""      - type: git
        url: https://github.com/thornbux/glimt.git
        tag: {tag}
        commit: {commit}
""")
# The comment at the top tells how to build from a checkout, which this is not.
text = text[text.index("id:"):]
os.makedirs(out, exist_ok=True)
with open(os.path.join(out, "io.github.thornbux.Glimt.yml"), "w") as f:
    f.write(f"# Glimt {tag}, made by tools/flathub-manifest.py in github.com/thornbux/glimt.\n")
    f.write(text)
shutil.copy("flatpak/cargo-sources.json", out)
print(f"{out}: io.github.thornbux.Glimt.yml ({tag}, {commit[:7]}) and cargo-sources.json")
