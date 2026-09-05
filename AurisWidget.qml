import QtCore
import QtQuick
import Quickshell
import Quickshell.Io
import qs.Common
import qs.Services
import qs.Widgets
import qs.Modules.Plugins

// auris: AirPods battery per bud and case, ear detection and noise control.
//
// All data comes from $XDG_RUNTIME_DIR/aurisd/state.json, which the aurisd
// daemon rewrites atomically (tmp file + rename). Control goes the other way
// through the `auris` CLI, which talks to the daemon over its own socket.
// The plugin owns no state of its own beyond the five options on its settings page.
PluginComponent {
    id: root

    property var popoutService: null

    // ---- settings ----------------------------------------------------------

    readonly property bool showPercent: pluginData.showPercent !== undefined ? pluginData.showPercent : true
    readonly property string pillValue: pluginData.pillValue !== undefined ? pluginData.pillValue : "buds"
    readonly property int lowThreshold: pluginData.lowThreshold !== undefined ? pluginData.lowThreshold : 20
    readonly property int criticalThreshold: pluginData.criticalThreshold !== undefined ? pluginData.criticalThreshold : 10
    readonly property bool hideWhenDisconnected: pluginData.hideWhenDisconnected !== undefined ? pluginData.hideWhenDisconnected : true
    readonly property bool popupOnConnect: pluginData.popupOnConnect !== undefined ? pluginData.popupOnConnect : true
    readonly property int popupSeconds: pluginData.popupSeconds !== undefined ? pluginData.popupSeconds : 6

    // Escape hatch for testing and for non-standard install prefixes: the CLI is
    // looked up on PATH unless pluginData.ctlCommand names something else.
    readonly property string ctlCommand: pluginData.ctlCommand ? pluginData.ctlCommand : "auris"

    // ---- data layer --------------------------------------------------------

    readonly property string runtimeDir: {
        const env = Quickshell.env("XDG_RUNTIME_DIR");
        if (env)
            return env;
        const loc = String(StandardPaths.writableLocation(StandardPaths.RuntimeLocation) || "");
        return loc.startsWith("file://") ? loc.substring(7) : loc;
    }
    readonly property string statePath: runtimeDir ? runtimeDir + "/aurisd/state.json" : ""

    // Last successfully parsed state.json, kept across failures so a dropped
    // daemon leaves the last known battery on screen rather than blanking it.
    property var st: null
    property bool daemonUp: false

    // 1 Hz heartbeat, so "Updated N s ago" moves on its own between file writes.
    property int tick: 0

    // Optimistic echo of a noise mode we asked for but have not seen confirmed.
    // Cleared when the daemon echoes the same mode back, or by pendingNoiseTimeout
    // if the accessory never confirms it.
    property string pendingNoise: ""

    function parseState(content) {
        if (!content) {
            daemonUp = false;
            return;
        }
        try {
            const obj = JSON.parse(content);
            if (!obj || typeof obj !== "object")
                return;
            const wasConnected = connected;
            const hadState = st !== null;
            st = obj;
            daemonUp = true;
            if (hadState && !wasConnected && connected && popupOnConnect)
                connectPopup.restart();
            if (pendingNoise && obj.noise_control === pendingNoise) {
                pendingNoise = "";
                pendingNoiseTimeout.stop();
            }
        } catch (e) {
            // Torn read of a file being replaced under us. Keep the old state;
            // the watch or the poll timer will bring the whole file along shortly.
        }
    }

    FileView {
        id: stateFile

        path: root.statePath
        blockWrites: true
        watchChanges: true
        printErrors: false
        onLoaded: root.parseState(text())
        onLoadFailed: error => {
            root.daemonUp = false;
        }
    }

    // The daemon replaces state.json with an atomic rename, which the file
    // watcher does not always follow, so poll as well: 2 s while the daemon
    // is up, 3 s while the file is missing. The file is under 1 KiB.
    Timer {
        interval: root.daemonUp ? 2000 : 3000
        repeat: true
        running: root.statePath !== ""
        onTriggered: stateFile.reload()
    }

    Timer {
        interval: 1000
        repeat: true
        running: true
        onTriggered: root.tick = (root.tick + 1) % 86400
    }

    // The accessory usually echoes a mode change within a few hundred ms. If it
    // never does, drop the optimistic value silently rather than lying forever.
    Timer {
        id: pendingNoiseTimeout

        interval: 5000
        repeat: false
        onTriggered: root.pendingNoise = ""
    }

    // ---- derived state -----------------------------------------------------

    readonly property var dev: st && st.device ? st.device : null
    readonly property var bat: st && st.battery ? st.battery : null
    readonly property var ear: st && st.ear ? st.ear : null

    readonly property bool connected: dev !== null && dev.connected === true
    readonly property string deviceName: dev && dev.name ? dev.name : "AirPods"
    readonly property string model: dev && dev.model ? dev.model : ""
    readonly property string firmware: dev && dev.firmware ? dev.firmware : ""
    readonly property string source: st && st.daemon && st.daemon.source ? st.daemon.source : "none"

    readonly property string noise: pendingNoise ? pendingNoise : (st && st.noise_control ? st.noise_control : "unknown")
    readonly property bool caKnown: st !== null && st.conversational_awareness !== null && st.conversational_awareness !== undefined
    readonly property bool ca: caKnown && st.conversational_awareness === true
    readonly property int adaptiveLevel: st && typeof st.adaptive_level === "number" ? st.adaptive_level : 0

    function slot(side) {
        if (!bat || !bat[side])
            return null;
        return bat[side];
    }
    // Last known level, kept by the daemon even after the component drops
    // out (the buds only relay the case while they sit in it).
    function level(side) {
        const s = slot(side);
        return s && typeof s.level === "number" ? s.level : -1;
    }
    function present(side) {
        const s = slot(side);
        return s !== null && s.present === true;
    }
    // Level only while the component is reporting right now; the bar pill
    // must not show a number that could be hours old.
    function liveLevel(side) {
        return present(side) ? level(side) : -1;
    }
    function seenCaption(side, heartbeat) {
        void heartbeat;
        const s = slot(side);
        if (!s || s.present === true)
            return "";
        if (typeof s.level !== "number")
            return "not seen yet";
        const secs = s.last_seen ? Math.round((Date.now() - Date.parse(s.last_seen)) / 1000) : NaN;
        if (!isFinite(secs) || secs < 0)
            return "last known";
        if (secs < 60)
            return "last seen just now";
        if (secs < 3600)
            return "last seen " + Math.round(secs / 60) + " min ago";
        if (secs < 86400)
            return "last seen " + Math.round(secs / 3600) + " h ago";
        return "last seen " + Math.round(secs / 86400) + " d ago";
    }
    function charging(side) {
        const s = slot(side);
        return s !== null && s.present === true && s.charging === true;
    }
    function earOf(side) {
        return ear && ear[side] ? ear[side] : "unknown";
    }

    readonly property int leftLevel: liveLevel("left")
    readonly property int rightLevel: liveLevel("right")
    readonly property int caseLevel: liveLevel("case")

    readonly property int budsMin: {
        if (leftLevel < 0)
            return rightLevel;
        if (rightLevel < 0)
            return leftLevel;
        return Math.min(leftLevel, rightLevel);
    }
    readonly property int allMin: {
        if (caseLevel < 0)
            return budsMin;
        if (budsMin < 0)
            return caseLevel;
        return Math.min(budsMin, caseLevel);
    }
    readonly property bool anyCharging: charging("left") || charging("right") || charging("case")

    readonly property int pillLevel: {
        switch (pillValue) {
        case "all":
            return allMin;
        case "left":
            return leftLevel;
        case "right":
            return rightLevel;
        case "case":
            return caseLevel;
        default:
            return budsMin;
        }
    }
    readonly property bool pillCharging: {
        switch (pillValue) {
        case "left":
            return charging("left");
        case "right":
            return charging("right");
        case "case":
            return charging("case");
        case "all":
            return anyCharging;
        default:
            return charging("left") || charging("right");
        }
    }

    // Seconds since the daemon last wrote the file, -1 when unknown.
    readonly property int ageSec: {
        tick;
        if (!st || !st.updated_at)
            return -1;
        const t = Date.parse(st.updated_at);
        if (isNaN(t))
            return -1;
        return Math.max(0, Math.round((Date.now() - t) / 1000));
    }

    // Deliberately not a function of ageSec: the daemon only rewrites state.json
    // when a field changes, so a healthy idle link produces an arbitrarily old
    // file. Staleness comes from the daemon's own flag instead.
    readonly property bool stale: !daemonUp || !connected || (bat !== null && bat.stale === true)

    function ageText() {
        if (ageSec < 0)
            return "never updated";
        if (ageSec < 60)
            return "Updated " + ageSec + " s ago";
        if (ageSec < 3600)
            return "Updated " + Math.round(ageSec / 60) + " min ago";
        return "Updated " + Math.round(ageSec / 3600) + " h ago";
    }

    readonly property string noiseLabel: {
        switch (noise) {
        case "off":
            return "Noise control off";
        case "anc":
            return "ANC";
        case "transparency":
            return "Transparency";
        case "adaptive":
            return "Adaptive";
        default:
            return "Noise control unknown";
        }
    }

    readonly property string statusLine: {
        if (!daemonUp)
            return "aurisd not running";
        if (!connected)
            return ageSec >= 0 ? "Disconnected, last seen " + Math.max(1, Math.round(ageSec / 60)) + " min ago" : "Disconnected";
        return "Connected, " + noiseLabel;
    }

    // ---- colour ------------------------------------------------------------
    //
    // Same ladder as BatteryService.levelColor, but against this plugin's own
    // thresholds. BatteryService itself is the laptop battery and must not be
    // reused here.
    readonly property color cautionColor: "#FFC107"
    readonly property color criticalColor: "#F44336"

    function levelColor(lvl, isCharging) {
        if (isCharging)
            return Theme.success;
        if (lvl < 0)
            return Theme.surfaceVariantText;
        if (lvl <= criticalThreshold)
            return criticalColor;
        if (lvl <= lowThreshold)
            return Theme.warning;
        if (lvl <= lowThreshold * 2)
            return cautionColor;
        return Theme.widgetTextColor;
    }
    function dimmed(c) {
        return stale ? Theme.withAlpha(c, 0.45) : c;
    }

    readonly property color pillColor: dimmed(levelColor(pillLevel, pillCharging))
    readonly property string pillIcon: connected && daemonUp ? "headphones" : "bluetooth_disabled"
    readonly property string pillText: pillLevel >= 0 ? pillLevel + "%" : "--"

    readonly property string noiseIcon: {
        switch (noise) {
        case "anc":
            return "noise_control_on";
        case "transparency":
            return "noise_aware";
        case "adaptive":
            return "blur_on";
        case "off":
            return "noise_control_off";
        default:
            return "headphones";
        }
    }

    // ---- control -----------------------------------------------------------

    readonly property var ctlProcIds: ({
        "noise": "auris.ctl.noise",
        "ca": "auris.ctl.ca",
        "adaptive": "auris.ctl.adaptive",
        "reconnect": "auris.ctl.reconnect"
    })

    function ctl(args) {
        // The shell service may run without ~/.local/bin on PATH; prepend it so a
        // per-user install of aurisd works as well as a system package.
        const argv = ["sh", "-c", "PATH=\"$HOME/.local/bin:$PATH\" exec \"$0\" \"$@\"", ctlCommand].concat(args);
        // Proc.runCommand coalesces calls sharing an id within 50 ms, so each
        // subcommand gets its own id or a quick pair of clicks loses one.
        const procId = ctlProcIds[args[0]] ? ctlProcIds[args[0]] : "auris.ctl." + args[0];
        Proc.runCommand(procId, argv, (stdout, exitCode) => {
            if (exitCode !== 0) {
                root.pendingNoise = "";
                pendingNoiseTimeout.stop();
                ToastService.showError("auris: " + args.join(" ") + " failed", String(stdout || "").trim());
                return;
            }
            stateFile.reload();
        });
    }

    function setNoise(mode) {
        if (!connected)
            return;
        pendingNoise = mode;
        pendingNoiseTimeout.restart();
        ctl(["noise", mode]);
    }
    function setConversationalAwareness(on) {
        ctl(["ca", on ? "on" : "off"]);
    }
    function setAdaptiveLevel(v) {
        ctl(["adaptive", String(Math.round(v))]);
    }
    function reconnect() {
        ctl(["reconnect"]);
    }

    readonly property var noiseModes: ["off", "anc", "transparency", "adaptive"]
    readonly property int noiseIndex: noiseModes.indexOf(noise)

    // ---- visibility --------------------------------------------------------
    //
    // conditionVisible is only consulted when a visibilityCommand is set, so the
    // hide-when-disconnected option goes through the override API instead.
    readonly property bool wantVisible: !(hideWhenDisconnected && !connected)

    onWantVisibleChanged: setVisibilityOverride(wantVisible)

    Timer {
        interval: 1
        repeat: false
        running: true
        onTriggered: root.setVisibilityOverride(root.wantVisible)
    }

    // ---- connect popup -----------------------------------------------------
    //
    // On the disconnected -> connected edge, open the stats popout for a few
    // seconds, the way macOS shows the battery card when AirPods connect. The
    // pill may have been hidden a moment ago, so wait one layout pass before
    // asking the base class to anchor the popout to it.
    Timer {
        id: connectPopup
        interval: 400
        repeat: false
        onTriggered: {
            if (!root.connected || (root.popoutRef && root.popoutRef.shouldBeVisible))
                return;
            root.triggerPopout();
            connectPopupClose.restart();
        }
    }

    Timer {
        id: connectPopupClose
        interval: root.popupSeconds * 1000
        repeat: false
        onTriggered: {
            if (root.popoutRef && root.popoutRef.shouldBeVisible)
                root.closePopout();
        }
    }

    // ---- bar ---------------------------------------------------------------

    // Right click flips between ANC and Transparency, the only two modes worth
    // swapping without looking at the panel.
    pillRightClickAction: () => root.setNoise(root.noise === "anc" ? "transparency" : "anc")

    horizontalBarPill: Component {
        Row {
            spacing: Theme.spacingXS

            DankIcon {
                anchors.verticalCenter: parent.verticalCenter
                name: root.pillIcon
                filled: root.connected && !root.stale
                size: root.iconSize
                color: root.pillColor
            }

            DankIcon {
                anchors.verticalCenter: parent.verticalCenter
                visible: root.anyCharging && root.connected
                name: "bolt"
                filled: true
                size: Theme.iconSizeSmall - 2
                color: root.dimmed(Theme.success)
            }

            StyledText {
                anchors.verticalCenter: parent.verticalCenter
                visible: root.showPercent && root.pillLevel >= 0
                text: root.pillText
                font.pixelSize: Theme.fontSizeSmall
                color: root.pillColor
            }
        }
    }

    verticalBarPill: Component {
        Column {
            spacing: Theme.spacingXS

            DankIcon {
                anchors.horizontalCenter: parent.horizontalCenter
                name: root.pillIcon
                filled: root.connected && !root.stale
                size: root.iconSize
                color: root.pillColor
            }

            StyledText {
                anchors.horizontalCenter: parent.horizontalCenter
                visible: root.showPercent && root.pillLevel >= 0
                text: String(root.pillLevel)
                font.pixelSize: Theme.fontSizeSmall
                color: root.pillColor
            }
        }
    }

    // ---- shared row --------------------------------------------------------
    //
    // One battery line: icon, label, track, bolt, percentage. DMS ships no
    // progress bar widget, so the track is a Rectangle with a second Rectangle
    // clipped inside it.
    // The Material Symbols font has no AirPods glyphs, so the three row icons
    // are drawn by hand: a left bud, a right bud (mirrored) and the case.
    component PodIcon: Canvas {
        id: pod

        property string kind: "left"
        property color color: Theme.surfaceText
        property int size: 20

        width: size
        height: size
        antialiasing: true
        onColorChanged: requestPaint()
        onKindChanged: requestPaint()
        onSizeChanged: requestPaint()

        onPaint: {
            const ctx = getContext("2d");
            const s = width;
            ctx.reset();
            ctx.clearRect(0, 0, width, height);
            ctx.fillStyle = pod.color;
            if (pod.kind === "case") {
                const w = s * 0.82, h = s * 0.66, x = (s - w) / 2, y = (s - h) / 2, r = s * 0.17;
                ctx.beginPath();
                ctx.roundedRect(x, y, w, h, r, r);
                ctx.fill();
                // carve the lid seam and the status light out of the body
                ctx.globalCompositeOperation = "destination-out";
                const seam = y + h * 0.4;
                ctx.fillRect(x, seam - s * 0.035, w, s * 0.07);
                ctx.beginPath();
                ctx.arc(s / 2, seam + h * 0.33, s * 0.075, 0, Math.PI * 2);
                ctx.fill();
                ctx.globalCompositeOperation = "source-over";
                return;
            }
            ctx.save();
            if (pod.kind === "right") {
                ctx.translate(s, 0);
                ctx.scale(-1, 1);
            }
            // oval head, tilted a touch, with a straight stem hanging off its inner side
            const hx = s * 0.4, hy = s * 0.32;
            ctx.save();
            ctx.translate(hx, hy);
            ctx.rotate(-0.35);
            ctx.beginPath();
            ctx.ellipse(-s * 0.27, -s * 0.2, s * 0.54, s * 0.4);
            ctx.fill();
            ctx.restore();
            ctx.beginPath();
            ctx.roundedRect(s * 0.47, s * 0.36, s * 0.19, s * 0.6, s * 0.095, s * 0.095);
            ctx.fill();
            ctx.restore();
        }
    }

    component BatteryRow: Item {
        id: batteryRow

        property string label: ""
        property string iconKind: "left"
        property int level: -1
        property bool charging: false
        property string caption: ""
        property bool dim: false

        readonly property bool hasCaption: caption.length > 0

        height: hasCaption ? 46 : 32
        opacity: dim ? 0.5 : 1

        Behavior on opacity {
            NumberAnimation {
                duration: Theme.shortDuration
                easing.type: Theme.standardEasing
            }
        }

        Behavior on height {
            NumberAnimation {
                duration: Theme.shortDuration
                easing.type: Theme.standardEasing
            }
        }

        Item {
            id: line

            anchors.left: parent.left
            anchors.right: parent.right
            anchors.top: parent.top
            height: 32

            PodIcon {
                id: rowIcon

                anchors.left: parent.left
                anchors.verticalCenter: parent.verticalCenter
                kind: batteryRow.iconKind
                size: Theme.iconSize - 2
                color: root.dimmed(Theme.surfaceText)
            }

            StyledText {
                id: rowLabel

                anchors.left: rowIcon.right
                anchors.leftMargin: Theme.spacingM
                anchors.verticalCenter: parent.verticalCenter
                width: 52
                text: batteryRow.label
                font.pixelSize: Theme.fontSizeSmall
                color: root.dimmed(Theme.surfaceText)
                elide: Text.ElideRight
            }

            StyledText {
                id: rowPercent

                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                width: 42
                horizontalAlignment: Text.AlignRight
                text: batteryRow.level >= 0 ? batteryRow.level + "%" : "--"
                font.pixelSize: Theme.fontSizeSmall
                color: root.dimmed(root.levelColor(batteryRow.level, batteryRow.charging))
            }

            DankIcon {
                id: rowBolt

                anchors.right: rowPercent.left
                anchors.rightMargin: Theme.spacingXS
                anchors.verticalCenter: parent.verticalCenter
                visible: batteryRow.charging
                name: "bolt"
                filled: true
                size: Theme.iconSizeSmall - 2
                color: root.dimmed(Theme.success)
            }

            Rectangle {
                id: rowTrack

                anchors.left: rowLabel.right
                anchors.leftMargin: Theme.spacingM
                anchors.right: rowBolt.visible ? rowBolt.left : rowPercent.left
                anchors.rightMargin: Theme.spacingM
                anchors.verticalCenter: parent.verticalCenter
                height: 6
                radius: height / 2
                color: Theme.withAlpha(Theme.surfaceVariantText, 0.22)

                Rectangle {
                    width: batteryRow.level >= 0 ? Math.max(parent.height, parent.width * batteryRow.level / 100) : 0
                    height: parent.height
                    radius: parent.radius
                    color: root.dimmed(root.levelColor(batteryRow.level, batteryRow.charging))

                    Behavior on width {
                        NumberAnimation {
                            duration: Theme.mediumDuration
                            easing.type: Theme.standardEasing
                        }
                    }
                }
            }
        }

        // Second line, under the label and bar, so it never crowds the numbers
        StyledText {
            anchors.left: parent.left
            anchors.leftMargin: rowIcon.width + Theme.spacingM
            anchors.right: parent.right
            anchors.top: line.bottom
            anchors.topMargin: -2
            visible: batteryRow.hasCaption
            text: batteryRow.caption
            font.pixelSize: Theme.fontSizeSmall - 1
            color: Theme.surfaceVariantText
            elide: Text.ElideRight
        }
    }

    function earCaption(side) {
        switch (earOf(side)) {
        case "out":
            return "out of ear";
        case "case":
            return "in case";
        case "in":
            return "";
        default:
            return connected ? "unknown" : "";
        }
    }

    // ---- panel -------------------------------------------------------------

    popoutWidth: 400
    popoutHeight: 460

    // The base class keeps its popout object private; PopoutComponent gets a
    // parentPopout reference when it is loaded, so pass it up here.
    property var popoutRef: null

    popoutContent: Component {
        PopoutComponent {
            onParentPopoutChanged: root.popoutRef = parentPopout
            id: popout

            headerText: root.deviceName
            detailsText: root.statusLine
            showCloseButton: true

            Column {
                width: parent.width
                spacing: Theme.spacingM

                StyledRect {
                    width: parent.width
                    height: rows.implicitHeight + Theme.spacingL * 2
                    radius: Theme.cornerRadius
                    color: Theme.floatingWindowNestedSurface
                    border.color: Theme.outlineMedium
                    border.width: Theme.layerOutlineWidth
                    opacity: root.stale ? 0.55 : 1

                    Column {
                        id: rows

                        anchors.fill: parent
                        anchors.margins: Theme.spacingL
                        spacing: Theme.spacingS

                        BatteryRow {
                            width: parent.width
                            label: "Left"
                            iconKind: "left"
                            level: root.level("left")
                            charging: root.charging("left")
                            caption: root.present("left") ? root.earCaption("left") : root.seenCaption("left", root.tick)
                            dim: !root.present("left") || (root.connected && root.earOf("left") !== "in")
                        }

                        BatteryRow {
                            width: parent.width
                            label: "Right"
                            iconKind: "right"
                            level: root.level("right")
                            charging: root.charging("right")
                            caption: root.present("right") ? root.earCaption("right") : root.seenCaption("right", root.tick)
                            dim: !root.present("right") || (root.connected && root.earOf("right") !== "in")
                        }

                        BatteryRow {
                            width: parent.width
                            label: "Case"
                            iconKind: "case"
                            level: root.level("case")
                            charging: root.charging("case")
                            caption: root.present("case") ? (root.st && root.st.lid === "open" ? "lid open" : "") : root.seenCaption("case", root.tick)
                            dim: !root.present("case")
                        }
                    }
                }

                StyledRect {
                    width: parent.width
                    height: controls.implicitHeight + Theme.spacingL * 2
                    radius: Theme.cornerRadius
                    color: Theme.floatingWindowNestedSurface
                    border.color: Theme.outlineMedium
                    border.width: Theme.layerOutlineWidth

                    Column {
                        id: controls

                        anchors.fill: parent
                        anchors.margins: Theme.spacingL
                        spacing: Theme.spacingM

                        // In single mode DankButtonGroup only emits; it never writes
                        // currentIndex itself, so this binding stays live.
                        DankButtonGroup {
                            model: ["Off", "ANC", "Transparency", "Adaptive"]
                            currentIndex: root.noiseIndex
                            selectionMode: "single"
                            size: "small"
                            enabled: root.connected
                            onSelectionChanged: (index, selected) => {
                                if (selected)
                                    root.setNoise(root.noiseModes[index]);
                            }
                        }

                        Column {
                            width: parent.width
                            spacing: Theme.spacingXS
                            visible: root.noise === "adaptive"

                            StyledText {
                                text: "Adaptive strength"
                                font.pixelSize: Theme.fontSizeSmall
                                color: Theme.surfaceVariantText
                            }

                            DankSlider {
                                width: parent.width
                                minimum: 0
                                maximum: 100
                                step: 5
                                unit: "%"
                                value: root.adaptiveLevel
                                enabled: root.connected
                                leftIcon: "blur_on"
                                onSliderDragFinished: finalValue => root.setAdaptiveLevel(finalValue)
                            }
                        }

                        DankToggle {
                            width: parent.width
                            height: 40
                            text: "Conversational awareness"
                            checked: root.ca
                            enabled: root.connected && root.caKnown
                            onToggled: isChecked => root.setConversationalAwareness(isChecked)
                        }
                    }
                }

                Column {
                    width: parent.width
                    spacing: Theme.spacingXS

                    StyledText {
                        width: parent.width
                        text: {
                            const bits = [];
                            if (root.model)
                                bits.push(root.model);
                            bits.push("via " + (root.source === "none" ? "no link" : root.source.toUpperCase()));
                            return bits.join("  ·  ");
                        }
                        font.pixelSize: Theme.fontSizeSmall
                        color: Theme.surfaceVariantText
                        elide: Text.ElideRight
                    }

                    StyledText {
                        width: parent.width
                        visible: root.firmware.length > 0
                        text: "firmware " + root.firmware
                        font.pixelSize: Theme.fontSizeSmall - 1
                        color: Theme.surfaceVariantText
                        elide: Text.ElideMiddle
                    }

                    StyledText {
                        text: root.ageText()
                        font.pixelSize: Theme.fontSizeSmall - 1
                        color: Theme.surfaceVariantText
                    }
                }

                DankButton {
                    visible: !root.connected
                    text: "Reconnect"
                    iconName: "refresh"
                    buttonHeight: 34
                    onClicked: root.reconnect()
                }
            }
        }
    }

    // ---- control centre ----------------------------------------------------

    ccWidgetIcon: connected ? noiseIcon : "bluetooth_disabled"
    ccWidgetPrimaryText: "AirPods"
    ccWidgetSecondaryText: {
        if (!daemonUp)
            return "aurisd not running";
        if (!connected)
            return "Disconnected";
        return (pillLevel >= 0 ? pillLevel + "%  ·  " : "") + noiseLabel;
    }
    ccWidgetIsActive: connected && !stale
    ccDetailHeight: 240

    onCcWidgetToggled: {
        if (connected)
            setNoise(noise === "anc" ? "transparency" : "anc");
        else
            reconnect();
    }

    ccDetailContent: Component {
        Rectangle {
            implicitHeight: 240
            radius: Theme.cornerRadius
            color: Theme.surfaceContainerHigh

            Column {
                anchors.fill: parent
                anchors.margins: Theme.spacingM
                spacing: Theme.spacingS

                Column {
                    width: parent.width
                    spacing: Theme.spacingXS
                    opacity: root.stale ? 0.55 : 1

                    BatteryRow {
                        width: parent.width
                        label: "Left"
                        iconKind: "left"
                        level: root.level("left")
                        charging: root.charging("left")
                        caption: root.present("left") ? root.earCaption("left") : root.seenCaption("left", root.tick)
                        dim: !root.present("left") || (root.connected && root.earOf("left") !== "in")
                    }

                    BatteryRow {
                        width: parent.width
                        label: "Right"
                        iconKind: "right"
                        level: root.level("right")
                        charging: root.charging("right")
                        caption: root.present("right") ? root.earCaption("right") : root.seenCaption("right", root.tick)
                        dim: !root.present("right") || (root.connected && root.earOf("right") !== "in")
                    }

                    BatteryRow {
                        width: parent.width
                        label: "Case"
                        iconKind: "case"
                        level: root.level("case")
                        charging: root.charging("case")
                        caption: root.seenCaption("case", root.tick)
                        dim: !root.present("case")
                    }
                }

                DankButtonGroup {
                    model: ["Off", "ANC", "Transparency", "Adaptive"]
                    currentIndex: root.noiseIndex
                    selectionMode: "single"
                    size: "small"
                    enabled: root.connected
                    onSelectionChanged: (index, selected) => {
                        if (selected)
                            root.setNoise(root.noiseModes[index]);
                    }
                }

                StyledText {
                    width: parent.width
                    text: root.statusLine
                    font.pixelSize: Theme.fontSizeSmall
                    color: Theme.surfaceVariantText
                    elide: Text.ElideRight
                }
            }
        }
    }

}
