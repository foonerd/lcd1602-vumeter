'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const boot = require('../lib/boot');

test('each bus mode names its bus and its boot lines', () => {
    assert.equal(boot.busFor('hardware'), 1);
    assert.equal(boot.busFor('hardware0'), 0);
    assert.equal(boot.busFor('software'), 3);
    assert.equal(boot.busFor('anything'), 1);
    assert.deepEqual(boot.linesFor('hardware'), []);
    assert.deepEqual(boot.linesFor('hardware0'), ['dtparam=i2c_vc=on']);
    assert.deepEqual(boot.linesFor('software', 17, 27), ['dtoverlay=i2c-gpio,bus=3,i2c_gpio_sda=17,i2c_gpio_scl=27,i2c_gpio_delay_us=2']);
    assert.deepEqual(boot.linesFor('software', 'x', 99), ['dtoverlay=i2c-gpio,bus=3,i2c_gpio_sda=23,i2c_gpio_scl=24,i2c_gpio_delay_us=2'], 'bad pins fall back');
});

test('the block is added, replaced and removed without touching other lines', () => {
    const original = 'dtoverlay=hifiberry-dac\n# a comment\n';
    const added = boot.rewrite(original, ['dtparam=i2c_vc=on']);
    assert.equal(added.changed, true);
    assert.equal(added.content, 'dtoverlay=hifiberry-dac\n# a comment\n# >>> lcd1602_vumeter\ndtparam=i2c_vc=on\n# <<< lcd1602_vumeter\n');
    const same = boot.rewrite(added.content, ['dtparam=i2c_vc=on']);
    assert.equal(same.changed, false);
    const replaced = boot.rewrite(added.content, ['dtoverlay=i2c-gpio,bus=3']);
    assert.equal(replaced.changed, true);
    assert.ok(replaced.content.includes('i2c-gpio'));
    assert.ok(!replaced.content.includes('i2c_vc'));
    const removed = boot.rewrite(replaced.content, []);
    assert.equal(removed.content, original);
    assert.equal(boot.rewrite('', []).changed, false, 'an empty file with nothing to add stays');
    assert.equal(boot.rewrite(undefined, ['x']).content, '# >>> lcd1602_vumeter\nx\n# <<< lcd1602_vumeter\n');
});
