'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');

// index.js needs Volumio's modules; only its pure helper is under test here.
const Module = require('module');
const original = Module._load;
Module._load = function (request, parent, isMain) {
    if (['kew', 'fs-extra', 'socket.io-client', 'v-conf'].includes(request)) {
        return {};
    }
    return original.call(this, request, parent, isMain);
};
const { parseI2cdetect } = require('../index');
Module._load = original;

test('the addresses in an i2cdetect table are read, the blanks and UU skipped', () => {
    const table = [
        '     0  1  2  3  4  5  6  7  8  9  a  b  c  d  e  f',
        '00:          -- -- -- -- -- -- -- -- -- -- -- -- --',
        '10: -- -- -- -- -- -- -- -- -- -- -- -- -- -- -- --',
        '20: -- -- -- -- -- -- -- 27 -- -- -- -- -- -- -- --',
        '30: -- -- -- -- -- -- -- -- -- -- -- -- -- -- 3e --',
        '40: -- -- -- -- -- -- -- -- -- -- -- -- -- -- -- --',
        '50: -- -- -- -- -- -- -- -- -- -- -- -- -- -- -- --',
        '60: 60 -- -- -- -- -- -- -- -- -- -- UU -- -- -- --',
        '70: -- -- -- -- -- -- -- --',
        '',
    ].join('\n');
    assert.deepEqual(parseI2cdetect(table), [0x27, 0x3e, 0x60]);
    assert.deepEqual(parseI2cdetect(''), []);
    assert.deepEqual(parseI2cdetect('00:          -- -- -- -- -- -- 09 -- -- -- -- -- --'), [0x09]);
});
