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
test('only the named account can control the named service lifecycle', () => {
    for (const verb of ['start', 'stop', 'restart']) {
        assert.equal(check('hasan', 'scour.service', verb), 'yes');
        assert.equal(check('guest', 'scour.service', verb), undefined);
        assert.equal(check('hasan', 'sshd.service', verb), undefined);
        assert.equal(check('hasan', undefined, verb), undefined);
    }
});
test('editing, transient units and other administrative actions remain gated', () => {
    for (const verb of ['set-property', 'reload', 'kill', 'reset-failed', undefined])
        assert.equal(check('hasan', 'scour.service', verb), undefined);
    for (const action of ['manage-unit-files', 'reload-daemon', 'set-environment'])
        assert.equal(check('hasan', 'scour.service', 'start', 'org.freedesktop.systemd1.' + action), undefined);
    assert.equal(check('hasan', 'scour.service', 'start', 'org.freedesktop.policykit.exec'), undefined);
});
