Scour @VERSION@ — Windows, EXPERIMENTAL
===========================================

The Windows build of a file search tool used daily on Linux. It has been run
on a few Windows machines; it is not tested the way the Linux build is.

No administrator needed, no Visual C++ Redistributable (statically linked).

INSTALL
-------
Double-click install.cmd. It copies Scour to %LOCALAPPDATA%\Programs\Scour,
adds Scour to the Start menu and that folder to your PATH, and opens the
window. The window asks once whether to start Scour with Windows; yes keeps
the index current between windows, and opens nothing at login.

Every fixed drive is indexed — C:, D: and the rest; USB sticks, DVDs and
network drives are not. The first scan walks each drive, so it takes a while.

Without installing: run scour-gui.exe from this folder. It starts the service
itself.

SETTINGS
--------
%APPDATA%\scour\config\config.toml, written on the first start. To index
other folders, list them as [[source]] entries: config-example.toml shows how.
A source listed there replaces the drives, so list everything you want.
Restart the service afterwards: close Scour, end scourd.exe in Task Manager,
open Scour again.

OTHER FACES
-----------
   scour            the command line:  scour report   scour *.pdf
   scour-tui        full-screen terminal
   scour-web        in the browser
   scour-mcp        MCP server, for AI tools

REMOVE
------
Run uninstall.cmd in %LOCALAPPDATA%\Programs\Scour. The index and settings
stay in %LOCALAPPDATA%\scour and %APPDATA%\scour; delete those as well for a
clean slate.

Untried on Windows: live watching beyond a few machines, network and FAT32
volumes. There is no USN journal reader yet: searching is as fast as you
would expect, the first scan is not.

Licence: MIT or Apache-2.0, both files beside this.
