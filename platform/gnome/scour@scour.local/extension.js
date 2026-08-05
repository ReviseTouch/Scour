/*
 * Show Scour.
 *
 * Thirty lines of JavaScript in a Rust project needs a reason, and it is this:
 * on GNOME under Wayland **nothing outside the compositor can raise a window**.
 * Every ordinary route was tried against a running Scour window first —
 *
 *   xdotool windowactivate      the X property never changed; denied
 *   org.gnome.Shell.FocusApp    AccessDenied, like Eval and Introspect
 *   launching the app again     opened a second window, which is worse
 *
 * — so a shortcut that is supposed to bring the window forward has to be
 * handled by something running inside the shell. That is all this does. It has
 * no preferences, no indicator and no keybinding of its own: the shortcut
 * lives in GNOME's own settings, where it can be seen and changed, and calls
 * the method below.
 *
 * `activate()` is one call for both cases — it focuses the window when the app
 * is running and launches it when it is not — because that is exactly what the
 * shell does when its own icon is clicked.
 */

import Gio from 'gi://Gio';
import Shell from 'gi://Shell';
import { Extension } from 'resource:///org/gnome/shell/extensions/extension.js';

const NAME = 'org.scour.Shell';
const PATH = '/org/scour/Shell';
const APP = 'scour.desktop';

const IFACE = `
<node>
  <interface name="org.scour.Shell">
    <method name="Show"/>
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

    Show() {
        const app = Shell.AppSystem.get_default().lookup_app(APP);
        if (!app) {
            logError(new Error(`${APP} is not installed`), 'scour');
            return;
        }
        app.activate();
    }
}
