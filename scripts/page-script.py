#!/usr/bin/env python3
"""The page's script, on its own, so that a parser can be pointed at it.

`apps/scour-web/src/page.html` is one file holding the markup, the style and
the program, and the program is the part nothing else checks: it is compiled
into the bridge as a string, so every Rust tool sees an opaque blob. A
duplicate declaration in it is a parse error, a parse error means the browser
refuses the *whole* script, and the window then comes up blank with nothing
reported anywhere. See `scripts/check`.
"""

import pathlib
import sys

page = pathlib.Path(__file__).resolve().parent.parent / "apps/scour-web/src/page.html"
text = page.read_text(encoding="utf8")
start = text.index("<script>") + len("<script>")
sys.stdout.write(text[start : text.index("</script>", start)])
