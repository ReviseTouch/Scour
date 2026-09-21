Scour @VERSION@ — Windows, EXPERIMENTAL
===========================================

The Windows build of a file search tool used daily on Linux. It has had one
afternoon on one Windows machine: the service indexed, the command line
searched, the query language answered. Not tested beyond that.

No installer, nothing written to the registry, no Visual C++ Redistributable
needed (statically linked).

1) Open config-ornek.toml, put your own folders in `roots`, save it as
   config.toml in this folder.
2) In a command prompt:   scourd.exe --config config.toml   (leave it open)
3) In another:            scour.exe report      scour.exe *.pdf
                          scour.exe "ext:xlsx size:>10mb"
   Five rows at a terminal; add  -n 40  for more.

Other faces, with the service running: scour-gui.exe (window), scour-tui.exe
(full-screen terminal), scour-web.exe (browser), scour-mcp.exe (MCP server).

Untried on Windows: live watching (set watch = false if it misbehaves; the
service rescans on a schedule anyway), the window, network and FAT32 volumes.
There is no USN journal reader yet, so the first scan walks; searching is as
fast as you would expect, the first scan is not.

Something wrong? Copy the lines from scourd.exe's window — the error is there.
Remove: delete this folder and the index folder (%LOCALAPPDATA%\scour unless
config says otherwise).  Licence: MIT or Apache-2.0, both files beside this.
