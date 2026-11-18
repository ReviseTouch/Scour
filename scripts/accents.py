#!/usr/bin/env python3
"""Writes crates/scour-core/src/accents.rs: the letters a search reads without
their marks. Run from the repository root: python3 scripts/accents.py"""
import unicodedata as u

# Letters that are another letter to a person and do not decompose.
SPECIAL = {
    "ß": "ss", "æ": "ae", "œ": "oe", "þ": "th", "ø": "o", "ł": "l",
    "đ": "d", "ð": "d", "ħ": "h", "ŧ": "t", "ı": "i",
}
# Where a combining mark is a diacritic and not a vowel: Indic and Thai marks
# are letters, and a kana voicing mark changes the sound.
MARK_BLOCKS = [
    (0x300, 0x36F), (0x483, 0x489), (0x590, 0x5FF), (0x600, 0x6FF),
    (0x1AB0, 0x1AFF), (0x1DC0, 0x1DFF), (0x20D0, 0x20FF), (0xFE20, 0xFE2F),
]
TATWEEL = 0x640


def alphabetic(c):
    try:
        # Letters only: a spacing accent is a sign, and loses nothing.
        return u.category(c).startswith("L") and u.name(c).startswith(("LATIN", "GREEK", "CYRILLIC"))
    except ValueError:
        return False


def bases():
    out = []
    for cp in range(0x80, 0x30000):
        c = chr(cp)
        if c in SPECIAL:
            out.append((cp, SPECIAL[c]))
            continue
        # The fold lowercases first, so only lowercase letters are looked up.
        if not alphabetic(c) or c.lower() != c:
            continue
        bare = "".join(x for x in u.normalize("NFD", c) if u.category(x) not in ("Mn", "Me"))
        if len(bare) != 1:
            continue
        bare = SPECIAL.get(bare, bare)
        if bare != c:
            out.append((cp, bare))
    return out


def marks():
    out = []
    for lo, hi in MARK_BLOCKS:
        for cp in range(lo, hi + 1):
            if u.category(chr(cp)) in ("Mn", "Me") or cp == TATWEEL:
                if out and out[-1][1] == cp - 1:
                    out[-1][1] = cp
                else:
                    out.append([cp, cp])
    return out


def table(name, doc, items):
    lines = [doc, "pub(crate) static %s: [(u32, u32); %d] = [" % (name, len(items))]
    line = "   "
    for a, b in items:
        item = " (0x%x, 0x%x)," % (a, b)
        if len(line) + len(item) > 96:
            lines.append(line)
            line = "   "
        line += item
    return lines + [line, "];"]


def main():
    packed = [
        (cp, ord(b) if len(b) == 1 else 0x80000000 | ord(b[0]) << 8 | ord(b[1]))
        for cp, b in bases()
    ]
    out = [
        "//! Which letters are another letter with a mark on it, and which characters",
        "//! are marks. Written by `scripts/accents.py` from Unicode %s; not edited." % u.unidata_version,
        "",
        "/// Set on a base that is two ASCII letters: `TWO | a << 8 | b`.",
        "pub(crate) const TWO: u32 = 0x8000_0000;",
        "",
    ]
    out += table(
        "BASES",
        "/// A lowercase letter and what it is without its mark, sorted by the letter.",
        packed,
    )
    out.append("")
    out += table(
        "MARKS",
        "/// Runs of combining marks a name is searched without: the diacritics of\n"
        "/// Latin, Greek and Cyrillic, Hebrew points, Arabic vowel signs, the tatweel.",
        marks(),
    )
    with open("crates/scour-core/src/accents.rs", "w", encoding="utf-8") as f:
        f.write("\n".join(out) + "\n")


if __name__ == "__main__":
    main()
