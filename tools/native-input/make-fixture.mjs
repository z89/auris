import fs from 'node:fs';
import path from 'node:path';

const [dms, out] = process.argv.slice(2);
if (!dms || !out) throw new Error('usage: make-fixture.mjs DMS_SOURCE FIXTURE');
function write(name, source) {
    const dest = path.join(out, name);
    fs.mkdirSync(path.dirname(dest), {recursive: true});
    fs.writeFileSync(dest, source);
}
function copy(name, from = name) { write(name, fs.readFileSync(path.join(dms, from))); }
write('shell.qml', fs.readFileSync(new URL('dms-shell.qml', import.meta.url)));
write('PopoutInputUpdates.qml', fs.readFileSync(new URL('../../components/PopoutInputUpdates.qml', import.meta.url)));
write('offscreen.qml', fs.readFileSync(new URL('shell.qml', import.meta.url)));
const singletons = {
    Theme: `
property int barHeight: 48
property int spacingL: 14
property int cornerRadius: 10
property bool isDirectionalEffect: false
property bool isDepthEffect: false
property bool elevationEnabled: false
property var elevationLevel3: ({})
property string elevationLightDirection: "autoBar"
property int popupDistance: 4
property real popupTransparency: 1
property color surfaceContainer: "#334455"
property int popoutAnimationDuration: 180
property real effectScaleCollapsed: 0.92
property int effectAnimOffset: 20
property var variantPopoutEnterCurve: [0.2, 0, 0, 1, 1, 1]
property var variantPopoutExitCurve: [0.2, 0, 0, 1, 1, 1]
property real variantOpacityDurationScale: 1
function snap(x, dpr) { return Math.round(x * dpr) / dpr; }
function px(x, dpr) { return snap(x, dpr); }
function barThickness() { return 44; }
function springPreset() { return {stiffness: 300, damping: 30}; }
function variantDuration(d) { return d; }
function variantCloseInterval(d) { return d + 60; }
function withAlpha(c, a) { return Qt.rgba(c.r, c.g, c.b, a); }
`,
    SettingsData: `
enum Position { Top, Bottom, Left, Right }
property bool popoutElevationEnabled: false
property bool reduceMotion: false
function frameEdgeInsetForSide() { return 0; }
function getAdjacentBarInfo() { return {leftBar: 0, topBar: 0, rightBar: 0, bottomBar: 0}; }
function getBarBounds(s) { return {x: 0, y: 0, width: s.width, height: 44, wingSize: 0}; }
`,
    Log: `function scoped() { return {debug: function(){}, info: function(){}, warn: function(){}}; }`,
    LayerShell: `function fromEnv(key, fallback) { return fallback; }`,
    KeyboardFocus: `function keyboardFocus() { return 0; }`,
    PopoutManager: `function showPopout() {} function hidePopout() {} function popoutChanged() {}`,
};
let commonModule = 'module qs.Common\n';
for (const [name, body] of Object.entries(singletons)) {
    write(`qs/Common/${name}.qml`, `pragma Singleton\nimport QtQuick\nQtObject {\n${body}\n}\n`);
    commonModule += `singleton ${name} 1.0 ${name}.qml\n`;
}
copy('qs/Common/SpringMotion.qml', 'Common/SpringMotion.qml');
commonModule += 'SpringMotion 1.0 SpringMotion.qml\n';
write('qs/Common/qmldir', commonModule);
write('qs/Services/qmldir', 'module qs.Services\nsingleton CompositorService 1.0 CompositorService.qml\nsingleton BlurService 1.0 BlurService.qml\n');
write('qs/Services/CompositorService.qml', `pragma Singleton
import QtQuick
QtObject {
function frameConfiguredForScreen() { return false; }
function usesConnectedFrameChromeForScreen() { return false; }
function getScreenScale() { return 1; }
}`);
write('qs/Services/BlurService.qml', `pragma Singleton
import QtQuick
QtObject { property color borderColor: "#8899aa"; property real borderWidth: 1 }`);
let widgetsModule = 'module qs.Widgets\n';
for (const name of ['DankPopoutStandalone', 'DismissZone', 'PopoutHoverBodyTracker']) {
    copy(`qs/Widgets/${name}.qml`, `Widgets/${name}.qml`);
    widgetsModule += `${name} 1.0 ${name}.qml\n`;
}
const stubs = {
    PopoutHoverDismiss: `Item {
property bool dismissEnabled
property bool dismissSuspended
property bool surfaceVisible
property real globalOffsetX
property real globalOffsetY
signal dismissRequested
function cancelPending() {}
function updateBodyHover(over) {}
function updateCursor(x,y) {}
}`,
    WindowBlur: `QtObject {
property var targetWindow
property real blurX
property real blurY
property real blurWidth
property real blurHeight
property real blurRadius
property bool clipEnabled
property real clipX
property real clipY
property real clipWidth
property real clipHeight
function kick() {}
}`,
    ElevationShadow: `Item {
property var level
property string direction
property real fallbackOffset
property real targetRadius
property color targetColor
property bool shadowEnabled
}`,
};
for (const [name, body] of Object.entries(stubs)) {
    write(`qs/Widgets/${name}.qml`, `import QtQuick\n${body}\n`);
    widgetsModule += `${name} 1.0 ${name}.qml\n`;
}
write('qs/Widgets/qmldir', widgetsModule);
