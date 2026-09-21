const assert = require('node:assert/strict');
const test = require('node:test');
const fs = require('node:fs');
const vm = require('node:vm');

let rule;
vm.runInNewContext(fs.readFileSync('packaging/50-scour-service.rules', 'utf8'), {
    polkit: {addRule: fn => {rule = fn;}, Result: {YES: 'yes'}},
});
const check = (user, unit, verb, id = 'org.freedesktop.systemd1.manage-units') =>
    rule({id, lookup: key => ({unit, verb})[key]}, {user});

test('each account controls its own instance and no other', () => {
    for (const verb of ['start', 'stop', 'restart']) {
        assert.equal(check('alice', 'scour@alice.service', verb), 'yes');
        assert.equal(check('bob', 'scour@bob.service', verb), 'yes');
        // The template's own name is not an instance, and neither is anyone else's.
        assert.equal(check('alice', 'scour@bob.service', verb), undefined);
        assert.equal(check('alice', 'scour@.service', verb), undefined);
        assert.equal(check('alice', 'scour.service', verb), undefined);
        assert.equal(check('alice', 'sshd.service', verb), undefined);
        assert.equal(check('alice', undefined, verb), undefined);
        // No subject is not every subject.
        assert.equal(check(undefined, 'scour@undefined.service', verb), undefined);
    }
});

test('editing, transient units and other administrative actions remain gated', () => {
    for (const verb of ['set-property', 'reload', 'kill', 'reset-failed', undefined])
        assert.equal(check('alice', 'scour@alice.service', verb), undefined);
    for (const action of ['manage-unit-files', 'reload-daemon', 'set-environment'])
        assert.equal(check('alice', 'scour@alice.service', 'start', 'org.freedesktop.systemd1.' + action), undefined);
    assert.equal(check('alice', 'scour@alice.service', 'start', 'org.freedesktop.policykit.exec'), undefined);
});

const unit = fs.readFileSync('packaging/scour@.service', 'utf8');
const installer = fs.readFileSync('packaging/install-service.sh', 'utf8');
const exec = unit.match(/^ExecStart=(.*)$/m)[1].split(/\s+/);

test('the instance is a user name, and the helper resolves the rest from it', () => {
    // %i is the account; uid, gid and home are the password database's answer,
    // never this file's, because a system unit's %h is root's home.
    assert.deepEqual(exec.slice(1, 3), ['--as', '%i']);
    assert.ok(!/@(USER|UID|HOME)@/.test(unit), 'no placeholder survives in a template');
    assert.ok(!/%h/.test(unit), '%h in a system unit is root, not the instance');
});

test('root execs only a root-owned path; the home path is after the drop', () => {
    assert.equal(exec[0], '/usr/local/libexec/scour/scour-watch');
    const sep = exec.indexOf('--');
    assert.ok(sep > 0, 'the command to run comes after --');
    assert.equal(exec[sep + 1], '~/.local/bin/scourd');
});

test('the roots come from a file the installer writes for that instance', () => {
    assert.ok(exec.includes('$SCOUR_ROOTS'), 'unquoted, so systemd splits it per root');
    assert.match(unit, /^EnvironmentFile=\/etc\/scour\/%i\.conf$/m);
    assert.match(installer, /"\/etc\/scour\/\$user\.conf"/);
    assert.match(installer, /SCOUR_ROOTS=/);
});

test('the installer enables an instance and retires the single-user unit', () => {
    assert.match(installer, /systemctl enable "scour@\$user\.service"/);
    assert.match(installer, /\/etc\/systemd\/system\/scour@\.service/);
    assert.match(installer, /systemctl disable --now scour\.service/);
    // Two writers race for the index lock, so the per-user unit is checked for.
    assert.match(installer, /\.config\/systemd\/user\/\*\.wants\/scourd\.service/);
});
