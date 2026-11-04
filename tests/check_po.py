"""Checks the translations against the template (`make check-po`):

- each language has every message of po/rinflow.pot, none fuzzy or empty,
  and nothing left over from older sources;
- a translation has the same {placeholders} as the English (the code fills
  them in by name: a missing or misspelt one shows up raw);
- menu items keep their mnemonic (one `_`), desktop keywords end with `;`;
- `translator-credits` names translators, or stays `translator-credits` to
  show none (not the application's name);
- msgfmt --check accepts the file.

Usage: check_po.py po/rinflow.pot po/ja.po po/zh_CN.po ..."""
import re
import subprocess
import sys


def entries(path):
    """[(msgid, msgstr, flags, references)] of the live entries of a po file."""
    found = []
    for block in open(path, encoding="utf-8").read().split("\n\n"):
        fields, key, flags, references = {}, None, "", ""
        for line in block.split("\n"):
            if line.startswith("#~"):
                fields["obsolete"] = True
                continue
            if line.startswith("#,"):
                flags += line[2:]
            if line.startswith("#:"):
                references += line[2:]
            match = re.match(r'^(msgctxt|msgid|msgid_plural|msgstr(?:\[\d+\])?) "(.*)"$', line)
            if match:
                key = match.group(1)
                fields[key] = match.group(2)
            elif line.startswith('"') and key:
                fields[key] += line[1:-1]
        if "obsolete" in fields:
            found.append((None, None, "obsolete", ""))
        elif "msgid" in fields and fields["msgid"]:
            found.append((fields["msgid"], fields.get("msgstr", fields.get("msgstr[0]", "")), flags, references))
    return found


def placeholders(text):
    return sorted(re.findall(r"\{[a-z_]+\}", text))


def main():
    template, catalogs = sys.argv[1], sys.argv[2:]
    wanted = {msgid for msgid, _, _, _ in entries(template)}
    problems = []
    for path in catalogs:
        seen = set()
        for msgid, msgstr, flags, references in entries(path):
            if msgid is None:
                problems.append(f"{path}: an obsolete (#~) entry is left; msgattrib --no-obsolete removes it")
                continue
            seen.add(msgid)
            where = f"{path}: {msgid[:60]!r}"
            if "fuzzy" in flags:
                problems.append(f"{where}: fuzzy")
            if not msgstr:
                problems.append(f"{where}: not translated")
                continue
            if placeholders(msgid) != placeholders(msgstr):
                problems.append(f"{where}: placeholders {placeholders(msgid)} became {placeholders(msgstr)}")
            if msgid.count("_") == 1 and msgid.startswith("_") and msgstr.count("_") != 1:
                problems.append(f"{where}: the menu mnemonic (_) is lost")
            if msgid == "translator-credits" and msgstr.strip().lower() == "rinflow":
                problems.append(f"{where}: the About dialog would credit “Rinflow” as translator; name the translators or leave it as translator-credits")
            if ".desktop.in" in references and msgid.endswith(";") and not msgstr.endswith(";"):
                problems.append(f"{where}: desktop keywords must end with ';'")
        for msgid in sorted(wanted - seen):
            problems.append(f"{path}: {msgid[:60]!r} is missing; run `make pot`")
        for msgid in sorted(seen - wanted):
            problems.append(f"{path}: {msgid[:60]!r} is no longer in the sources; run `make pot`")
        result = subprocess.run(["msgfmt", "--check", "-o", "/dev/null", path], capture_output=True, text=True)
        if result.returncode != 0:
            problems.append(f"{path}: msgfmt --check: {result.stderr.strip()}")
    for problem in problems:
        print(problem)
    print(f"{len(catalogs)} languages, {len(wanted)} messages: {'OK' if not problems else f'{len(problems)} problems'}")
    sys.exit(1 if problems else 0)


if __name__ == "__main__":
    main()
