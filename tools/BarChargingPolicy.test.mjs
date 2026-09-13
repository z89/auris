import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

// Exercise the actual pure QML policy without loading the live shell or socket.
const source = readFileSync(new URL('../AurisWidget.qml', import.meta.url), 'utf8');
const body = source.match(/function barBudPresent\(side\)\s*\{([^}]+)\}/)?.[1];
assert.ok(body, 'bar availability policy must remain testable');
const policy = new Function('side', 'connected', 'cellLive', 'charging', 'earOf', body);
const levelExpression = side => source.match(new RegExp(`readonly property int ${side}Level: ([^\\n]+)`))[1];
const evaluateLevel = Object.fromEntries(['left', 'right'].map(side => [side,
    new Function('barBudPresent', 'level', `return ${levelExpression(side)};`)
]));

function bar(state) {
    const available = side => policy(side,
        state.connected !== false,
        name => state.stale !== true && state[name]?.present === true,
        name => state[name]?.charging === true,
        name => state[name]?.ear ?? 'unknown');
    return {
        left: available('left'),
        right: available('right'),
        leftLevel: evaluateLevel.left(available, side => state[side].level),
        rightLevel: evaluateLevel.right(available, side => state[side].level)
    };
}

const inUse = level => ({ present: true, charging: false, ear: 'in', level });

for (const chargingSide of ['left', 'right']) {
    test(`${chargingSide} charging before its ear event cannot affect the other bud`, () => {
        const state = { left: inUse(40), right: inUse(80) };
        const other = chargingSide === 'left' ? 'right' : 'left';
        assert.equal(bar(state)[chargingSide], true);
        state[chargingSide].charging = true;
        assert.equal(bar(state)[chargingSide], false);
        assert.equal(bar(state)[`${chargingSide}Level`], -1);
        assert.equal(bar(state)[other], true);
        assert.equal(bar(state)[`${other}Level`], state[other].level);
        state[chargingSide].ear = 'case';
        state[chargingSide].charging = false; // charge report ends before presence
        assert.equal(bar(state)[chargingSide], false);
        state[chargingSide].present = false;
        assert.equal(bar(state)[chargingSide], false);
        state[chargingSide] = inUse(95);
        assert.equal(bar(state)[chargingSide], true);
    });
}

test('unknown, absent, and full-but-in-case buds are not invented on the bar', () => {
    assert.deepEqual(bar({}), { left: false, right: false, leftLevel: -1, rightLevel: -1 });
    assert.equal(bar({ left: { ...inUse(100), ear: 'case' } }).left, false);
});

test('BLE-only observations do not make the bar look audio-connected', () => {
    assert.equal(bar({ connected: false, left: inUse(80) }).left, false);
});

test('stale retained cells are never presented as live bar readings', () => {
    assert.deepEqual(bar({ stale: true, left: inUse(40), right: inUse(80) }),
        { left: false, right: false, leftLevel: -1, rightLevel: -1 });
});

test('new per-cell freshness overrides the legacy battery-wide fallback', () => {
    assert.match(source, /function cellFresh\(side\)/);
    assert.match(source, /typeof s\.fresh === "boolean"/);
    assert.match(source, /return s\.fresh/);
    assert.match(source, /return !\(bat && bat\.stale === true\)/);
    assert.match(source, /return cellFresh\(side\);/);
    assert.match(source, /BLE observed · audio link not implied/);
});

test('paired bar buds overlap transparent canvases for a 1–2 px visible gap', () => {
    const gap = Number(source.match(/readonly property real visibleBudGap: ([\d.]+)/)?.[1]);
    const silhouette = source.match(/pairedOffset: budSize \* \(([-\d.]+) - ([-\d.]+)\) \+ visibleBudGap/);
    assert.ok(Number.isFinite(gap) && gap >= 1 && gap <= 2, 'visible gap must be 1–2 px');
    assert.ok(silhouette, 'paired offset must use visible silhouette edges');
    assert.equal(Number(silhouette[1]), 0.662);
    assert.equal(Number(silhouette[2]), 0.338);
    assert.match(source, /x: pillPods\.showBoth \? pillPods\.pairedOffset/);
});

test('bar has no charging presentation; panel retains it', () => {
    const barSource = source.slice(source.indexOf('component PillPodsIcon:'), source.indexOf('// ---- shared row'));
    assert.doesNotMatch(barSource, /name: "bolt"|Theme\.success|kind: "case"|waitingForPresence/);
    assert.match(barSource, /showLeft: root\.barBudPresent\("left"\)/);
    assert.match(barSource, /showRight: root\.barBudPresent\("right"\)/);
    assert.match(source, /pillColor: dimmed\(levelColor\(pillLevel, false\)\)/);
    assert.match(source, /visible: batteryRow\.charging/);
    assert.match(source, /name: "bolt"/);
});
