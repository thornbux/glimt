#!/usr/bin/env python3
"""Writes po/glimt.pot, the template for a new translation, from po/no_nb.po.

Every text of the interface is in no_nb.po first (AGENTS.md, Language), so the template is that
file with each translation left empty. lang::tests::the_template_has_every_text fails until it
is written again after a text is added or changed.
"""
import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parent.parent
HEADER = r'''# Glimt's interface: the template for a new language. Written by tools/pot.py from
# po/no_nb.po; do not edit it, but copy it.
#
# To translate Glimt into a new language:
# 1. Copy this file to po/<code>.po, where <code> is the language's code (pt, cs, ja …).
# 2. Fill in Language and Plural-Forms below (the gettext manual lists the rule of each
#    language), and write the translation of each msgid in its msgstr. A msgstr left empty
#    shows in English.
# 3. Keep {name} placeholders, %field% names and the key names (Ctrl, Shift, Enter, Esc,
#    Delete) as they are. A noun with msgid_plural gets as many msgstr[n] as the rule has forms.
# 4. Open a pull request, or an issue with the file attached. Adding the language to the
#    language menu (lang::LANGUAGES and its plural rule) can be left to the maintainer.
msgid ""
msgstr ""
"Project-Id-Version: Glimt\n"
"Language: \n"
"MIME-Version: 1.0\n"
"Content-Type: text/plain; charset=UTF-8\n"
"Plural-Forms: nplurals=INTEGER; plural=EXPRESSION;\n"
'''

nb = (ROOT / "po/no_nb.po").read_text(encoding="utf-8")
# The entries start after the header, at the first blank line.
entries = nb[nb.index("\n\n") + 1 :]
out, in_msgstr = [], False
for line in entries.split("\n"):
    if line.startswith("msg"):
        in_msgstr = line.startswith("msgstr")
    elif line.startswith('"') and in_msgstr:
        continue  # the rest of a translation, left empty too
    out.append(re.sub(r'^(msgstr(\[\d+\])?) ".*"$', r'\1 ""', line))
(ROOT / "po/glimt.pot").write_text(HEADER + "\n".join(out), encoding="utf-8")
