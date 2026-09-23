/*
 * Bring an open Scour window to the front.
 *
 * On GNOME under Wayland nothing outside the compositor can raise a window:
 * `xdotool windowactivate` changes nothing, `org.gnome.Shell.FocusApp` is
 * AccessDenied, and starting the program again opens a second window. So the
 * key that opens Scour asks the shell first, through the one method below; it
 * has no preferences, no indicator and no keybinding of its own.
 *
 * `Raise` never starts anything. Finding no window it answers false and the
 * caller, `scour-open`, starts the face; activating the app instead would run
 * the desktop entry, which is `scour-open` again.
 */

import Meta from 'gi://Meta';
import Gio from 'gi://Gio';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import { Extension } from 'resource:///org/gnome/shell/extensions/extension.js';

const NAME = 'org.scour.Shell';
const PATH = '/org/scour/Shell';

// The window's app id. The browser's window is found by its process instead:
// under `--app` Chromium names it after the page, not after `--class`.
const CLASSES = ['scour', 'com.revisetouch.scour'];

const IFACE = `
<node>
  <interface name="org.scour.Shell">
    <method name="Raise">
      <arg type="au" direction="in" name="pids"/>
      <arg type="s" direction="in" name="title"/>
      <arg type="b" direction="out" name="raised"/>
    </method>
  </interface>
</node>`;

export default class ScourExtension extends Extension {
    enable() {
        this._dbus = Gio.DBusExportedObject.wrapJSObject(IFACE, this);
        this._dbus.export(Gio.DBus.session, PATH);
        this._owner = Gio.bus_own_name(
            Gio.BusType.SESSION, NAME, Gio.BusNameOwnerFlags.NONE, null, null, null);
    }

    disable() {
        if (this._owner)
            Gio.bus_unown_name(this._owner);
        this._dbus?.unexport();
        this._dbus = null;
        this._owner = null;
    }

    // The most recently used Scour window, on any workspace, minimised or not.
    // `pids` are the faces' processes and, for the terminal face, the
    // processes above it; one terminal process can own many windows, and
    // `title` picks the one showing Scour. A title alone never matches.
    Raise(pids, title) {
        const windows = global.display.get_tab_list(Meta.TabList.NORMAL_ALL, null)
            .filter(w => pids.includes(w.get_pid())
                || CLASSES.includes((w.get_wm_class() ?? '').toLowerCase()));
        const ours = windows.find(w => title && w.get_title() === title) ?? windows[0];
        if (!ours)
            return false;
        Main.activateWindow(ours, global.display.get_current_time_roundtrip());
        return true;
    }
}
