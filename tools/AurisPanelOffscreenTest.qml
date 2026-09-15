import QtQuick
import QtQuick.Window
import qs.Common
import qs.Services

Window {
    id: testWindow

    property bool failed: false
    property int phase: 0
    property int ticks: 0
    property var widget: widgetLoader.item
    property var panel: panelLoader.item
    property real collapsedTechnicalHeight: 0
    property real collapsedPanelHeight: 0
    property bool captureStarted: false
    property real quickControlsY: 0

    function check(condition, message) {
        if (!condition) {
            console.error("FAIL: " + message);
            failed = true;
        }
    }

    function findNamed(item, name) {
        if (!item)
            return null;

        if (item.objectName === name)
            return item;

        const descendants = item.children || [];
        for (let i = 0; i < descendants.length; i++) {
            const found = findNamed(descendants[i], name);
            if (found)
                return found;
        }
        return null;
    }

    function containsText(item, expected) {
        if (!item)
            return false;

        if (typeof item.text === "string" && item.text === expected)
            return true;

        const descendants = item.children || [];
        for (let i = 0; i < descendants.length; i++) {
            if (containsText(descendants[i], expected))
                return true;
        }
        return false;
    }

    function findSettingsLoader(item) {
        const named = findNamed(item, "aurisSettingsLoader");
        if (named)
            return named;

        if (!item)
            return null;

        if (typeof item.status === "number" && typeof item.setSource === "function")
            return item;

        const descendants = item.children || [];
        for (let i = 0; i < descendants.length; i++) {
            const found = findSettingsLoader(descendants[i]);
            if (found)
                return found;
        }
        return null;
    }

    function connectedSnapshot(settingsApi) {
        const snapshot = {
            "schema": 1,
            "updated_at": new Date().toISOString(),
            "daemon": {
                "version": "headless",
                "source": "aap"
            },
            "device": {
                "address": "AA:BB:CC:DD:EE:FF",
                "name": "Headless AirPods",
                "model_id": "201B",
                "model": "AirPods 4 (ANC)",
                "firmware": "7B21",
                "connected": true,
                "aap_link": true
            },
            "battery": {
                "stale": false,
                "left": {
                    "level": 87,
                    "charging": false,
                    "present": true
                },
                "right": {
                    "level": 85,
                    "charging": true,
                    "present": true
                },
                "case": {
                    "level": 62,
                    "charging": true,
                    "present": true
                }
            },
            "ear": {
                "left": "in",
                "right": "case"
            },
            "lid": "open",
            "noise_control": "adaptive",
            "conversational_awareness": true,
            "adaptive_level": 45
        };
        if (settingsApi) {
            // settings_api 3 verifies its own writes, a rename among them:
            // every snapshot carries the requested values and what
            // verification made of them.
            snapshot.settings_api = 3;
            snapshot.settings = {
                "microphone": "auto",
                "press_speed": "default",
                "hold_duration": "default",
                "listening_mode_cycle": ["anc", "transparency", "adaptive"],
                "call_controls": "mute_once_hangup_twice",
                "personalized_volume": true
            };
            snapshot.settings_requested = {};
            snapshot.settings_status = {};
            snapshot.settings_verify = "idle";
            snapshot.verify_reopen = false;
        }
        return JSON.stringify(snapshot);
    }

    function finish() {
        watchdog.stop();
        Qt.exit(failed ? 1 : 0);
    }

    width: 420
    height: 760
    visible: true
    color: "#20242c"

    QtObject {
        id: fakeScreen

        property real height: testWindow.height
    }

    QtObject {
        id: fakePopout

        property bool shouldBeVisible: true
    }

    Loader {
        id: widgetLoader

        active: true
        source: Qt.resolvedUrl("../AurisWidget.qml")
        onLoaded: {
            testWindow.widget.parentScreen = fakeScreen;
            testWindow.widget.parseState(testWindow.connectedSnapshot(true));
            panelLoader.sourceComponent = testWindow.widget.popoutContent;
        }
    }

    Loader {
        id: panelLoader

        width: testWindow.width
        visible: true
        onLoaded: item.parentPopout = fakePopout
    }

    Timer {
        id: watchdog

        interval: 20
        repeat: true
        running: true
        onTriggered: {
            testWindow.ticks++;
            if (testWindow.ticks > 250) {
                console.error("FAIL: full-panel offscreen test timed out in phase " + testWindow.phase);
                testWindow.failed = true;
                testWindow.finish();
                return;
            }
            if (testWindow.phase === 0) {
                if (widgetLoader.status === Loader.Error) {
                    console.error("FAIL: AurisWidget failed to load");
                    testWindow.failed = true;
                    testWindow.finish();
                    return;
                }
                if (!testWindow.panel)
                    return;

                const settingsLoader = testWindow.findSettingsLoader(testWindow.panel);
                if (!settingsLoader || settingsLoader.status !== Loader.Ready || !settingsLoader.item)
                    return;

                const mainScroll = testWindow.findNamed(testWindow.panel, "aurisMainScroll");
                const technical = testWindow.findNamed(testWindow.panel, "aurisTechnicalDisclosure");
                const batteryLeft = testWindow.findNamed(testWindow.panel, "batteryLeft");
                const batteryRight = testWindow.findNamed(testWindow.panel, "batteryRight");
                const batteryCase = testWindow.findNamed(testWindow.panel, "batteryCase");
                const adaptiveSlider = testWindow.findNamed(testWindow.panel, "aurisAdaptiveSlider");
                const quickControls = testWindow.findNamed(testWindow.panel, "aurisQuickControls");
                const adaptiveControl = testWindow.findNamed(testWindow.panel, "aurisAdaptiveControl");
                const disclosureButton = testWindow.findNamed(testWindow.panel, "aurisSetupDisclosureButton");
                testWindow.check(quickControls !== null && quickControls.visible, "everyday controls are not immediately visible");
                testWindow.quickControlsY = quickControls ? quickControls.y : -1;
                testWindow.check(!settingsLoader.visible, "one-time setup is visible while its disclosure is collapsed");
                testWindow.check(settingsLoader.height > 0 && settingsLoader.item.implicitHeight > 0, "settings Loader lost its content height in the real panel nesting");
                testWindow.check(mainScroll !== null, "main panel scroll seam is missing");
                testWindow.check(technical !== null, "technical disclosure seam is missing");
                testWindow.check(disclosureButton !== null && disclosureButton.tooltipText === null, "disclosure chevron owns a hover tooltip");
                testWindow.check(testWindow.containsText(testWindow.panel, "UI revision") && testWindow.containsText(testWindow.panel, testWindow.widget.uiRevision), "technical information is missing the loaded UI revision");
                testWindow.check(batteryLeft !== null && batteryLeft.level === 87 && !batteryLeft.charging && batteryLeft.caption === "" && !batteryLeft.dim, "left battery row lost its reported state");
                testWindow.check(batteryRight !== null && batteryRight.level === 85 && batteryRight.charging && batteryRight.caption === "in case · charging" && !batteryRight.dim, "right battery row lost charging/in-case state");
                testWindow.check(batteryCase !== null && batteryCase.level === 62 && batteryCase.charging && batteryCase.caption === "charging" && !batteryCase.dim, "case battery row lost charging/lid state");
                testWindow.check(adaptiveSlider !== null && adaptiveSlider.enabled && adaptiveSlider.value === 45, "Adaptive slider did not expose the connected Adaptive state");

                // The daemon reports one status per key. The widget shows the
                // requested value while that is what the daemon is carrying, and
                // repeats the daemon's verdict rather than inventing one.
                const verified = JSON.parse(testWindow.connectedSnapshot(true));
                verified.settings.press_speed = "slower";
                verified.settings_requested = {
                    "microphone": "right",
                    "press_speed": "slower",
                    "hold_duration": "shorter",
                    "listening_mode_cycle": ["off", "anc"],
                    "call_controls": "hangup_once_mute_twice",
                    "personalized_volume": false
                };
                verified.settings_requested["name"] = "Auris Pods";
                verified.settings_status = {
                    "microphone": "verifying",
                    "press_speed": "confirmed",
                    "hold_duration": "mismatch",
                    "listening_mode_cycle": "unreported",
                    "call_controls": "unverified",
                    "name": "mismatch"
                };
                verified.settings_verify = "reopening";
                testWindow.widget.parseState(JSON.stringify(verified));
                testWindow.check(testWindow.widget.effectiveSetting("microphone") === "right", "a write being verified snapped back to the old device value");
                testWindow.check(testWindow.widget.effectiveSetting("press_speed") === "slower", "a confirmed write did not stay on screen");
                testWindow.check(testWindow.widget.effectiveSetting("hold_duration") === "default", "a mismatch did not fall back to the value the AirPods kept");
                testWindow.check(testWindow.widget.effectiveSetting("call_controls") === "hangup_once_mute_twice", "an unverified write was withdrawn from the control");
                testWindow.check(testWindow.widget.effectiveSetting("personalized_volume") === true, "a key with no status stopped showing the device value");
                const captions = testWindow.widget.advancedCaptions;
                testWindow.check(captions.microphone.text === "Verifying with AirPods\u2026", "verifying caption is wrong: " + captions.microphone.text);
                testWindow.check(captions.press_speed.text === "Confirmed", "confirmed caption is wrong: " + captions.press_speed.text);
                testWindow.check(captions.hold_duration.text === "AirPods kept Default", "mismatch caption does not name the value the AirPods kept: " + captions.hold_duration.text);
                testWindow.check(captions.listening_mode_cycle.text === "Applied. AirPods 4 doesn't report this setting back.", "unreported caption is wrong: " + captions.listening_mode_cycle.text);
                testWindow.check(captions.call_controls.text === "Sent, not verified", "unverified caption is wrong: " + captions.call_controls.text);
                testWindow.check(captions.personalized_volume === undefined, "a key with no status was given a caption");
                testWindow.check(String(captions.hold_duration.color) === String(Theme.error), "a mismatch is not in the error colour");
                testWindow.check(String(captions.call_controls.color) === String(Theme.warning), "an unverified write is not in the warning colour");
                testWindow.check(String(captions.microphone.color) === String(Theme.surfaceVariantText), "an ordinary caption borrowed a semantic colour");
                // A rename is verified like any other setting, under the
                // daemon's "name" key, and names what the AirPods kept.
                testWindow.check(captions.rename !== undefined && captions.rename.text === "AirPods kept " + testWindow.widget.confirmedDeviceName, "a rename mismatch did not name the device's own name: " + JSON.stringify(captions.rename));
                testWindow.check(captions.name === undefined, "the daemon's name key leaked into the panel's caption map");
                const renamed = JSON.parse(JSON.stringify(verified));
                renamed.settings_status.name = "confirmed";
                renamed.device.name = "Auris Pods";
                testWindow.widget.parseState(JSON.stringify(renamed));
                testWindow.check(testWindow.widget.advancedCaptions.rename.text === "Confirmed", "a confirmed rename was not reported by the daemon's status");
                testWindow.check(testWindow.widget.settingStatusOf("rename") === "confirmed", "the rename status did not reach the widget");
                const legacyRename = JSON.parse(JSON.stringify(renamed));
                legacyRename.settings_api = 2;
                testWindow.widget.parseState(JSON.stringify(legacyRename));
                testWindow.check(testWindow.widget.advancedCaptions.rename === undefined, "a daemon that cannot verify a rename still captioned one");
                testWindow.widget.parseState(JSON.stringify(verified));

                const settingsItem = settingsLoader.item;
                testWindow.check(testWindow.findNamed(settingsItem, "settingCaption_hold_duration") !== null, "the settings component has no caption lane");
                testWindow.check(testWindow.findNamed(settingsItem, "settingCaption_rename") !== null, "the rename control has no caption lane");

                // The daemon drops the link for about a second to read a write
                // back. Nothing on screen may call that a disconnect.
                const beforeReopen = {
                    "pill": testWindow.widget.pillLevel,
                    "height": testWindow.panel.implicitHeight,
                    "quickY": quickControls ? quickControls.y : -1
                };
                const reopen = JSON.parse(JSON.stringify(verified));
                reopen.verify_reopen = true;
                reopen.device.connected = false;
                reopen.device.aap_link = false;
                reopen.battery.stale = true;
                testWindow.widget.parseState(JSON.stringify(reopen));
                testWindow.check(testWindow.widget.connected, "a verification reopen was rendered as a disconnect");
                testWindow.check(!testWindow.widget.deviceConnected, "the raw device connection state was lost");
                testWindow.check(testWindow.widget.verifyReopen, "verify_reopen did not reach the widget");
                testWindow.check(testWindow.widget.wantVisible, "the bar pill dropped out for a verification reopen");
                testWindow.check(testWindow.widget.advancedAvailable, "the settings controls were disabled by a verification reopen");
                testWindow.check(!testWindow.widget.stale, "a verification reopen dimmed the bar pill");
                testWindow.check(testWindow.widget.pillLevel === beforeReopen.pill, "the bar pill lost its level during a verification reopen");
                testWindow.check(testWindow.widget.effectiveSetting("microphone") === "right", "a verification reopen discarded the value being verified");
                testWindow.check(testWindow.panel.implicitHeight === beforeReopen.height, "a verification reopen changed the panel height");
                testWindow.check(!quickControls || quickControls.y === beforeReopen.quickY, "a verification reopen moved the everyday controls");

                // An older daemon cannot verify anything, so the widget reports
                // device values and says nothing at all about the write.
                const legacy = JSON.parse(JSON.stringify(verified));
                legacy.settings_api = 1;
                testWindow.widget.parseState(JSON.stringify(legacy));
                testWindow.check(Object.keys(testWindow.widget.advancedCaptions).length === 0, "a daemon that cannot verify still produced captions");
                testWindow.check(testWindow.widget.effectiveSetting("microphone") === "auto", "a daemon that cannot verify did not fall back to device values");
                testWindow.check(testWindow.widget.effectiveSetting("hold_duration") === "default", "a daemon that cannot verify showed a requested value");
                testWindow.widget.parseState(testWindow.connectedSnapshot(true));

                // Drive the actual Proc callbacks: a rapid revert is queued
                // behind the send, then a post-ack click starts immediately.
                Proc.controlled = true;
                Proc.reset();
                testWindow.widget.requestAdvancedSetting("microphone", "left");
                testWindow.widget.requestAdvancedSetting("microphone", "auto");
                testWindow.check(Proc.calls.length === 1 && testWindow.widget.pendingAdvanced.microphone.queued.target === "auto", "rapid return to the reported value was not queued");
                Proc.complete(0, "", 0);
                testWindow.check(Proc.calls.length === 2 && testWindow.widget.pendingAdvanced.microphone.target === "auto", "queued return value was not sent after the first callback");
                Proc.complete(1, "", 0);
                testWindow.check(testWindow.widget.pendingAdvanced.microphone.inFlightSerial === 0, "successful callback left the key permanently in flight");
                testWindow.widget.requestAdvancedSetting("microphone", "right");
                testWindow.check(Proc.calls.length === 3 && testWindow.widget.pendingAdvanced.microphone.target === "right", "a later click after acknowledgement did not start a second send");
                testWindow.widget.requestAdvancedSetting("press_speed", "slower");
                testWindow.check(Proc.calls.length === 4 && testWindow.widget.pendingAdvanced.press_speed !== undefined, "a different key was blocked by microphone activity");
                testWindow.widget.clearAdvancedPending("microphone");
                Proc.complete(2, "late failure", 1);
                testWindow.check(testWindow.widget.pendingAdvanced.microphone === undefined, "old callback recreated a cancelled request");
                Proc.complete(3, "", 0);
                testWindow.widget.requestAdvancedSetting("microphone", "left");
                testWindow.widget.requestAdvancedSetting("microphone", "right");
                const beforeRejected = Proc.calls.length;
                Proc.complete(beforeRejected - 1, "request context changed", 1);
                testWindow.check(Proc.calls.length === beforeRejected && testWindow.widget.pendingAdvanced.microphone === undefined, "failed command automatically replayed a queued target");

                // A rename is a daemon-verified write like any other: the local
                // entry is retired by the status, not by the reported name
                // turning up whenever BlueZ next reads one.
                Proc.reset();
                testWindow.widget.requestAdvancedRename("Airpods55655567");
                testWindow.check(Proc.calls.length === 1, "a rename did not start a command");
                Proc.complete(0, "", 0);
                testWindow.check(testWindow.widget.pendingAdvanced.rename !== undefined, "a rename stopped waiting for the daemon's verdict");
                const renameVerdict = JSON.parse(testWindow.connectedSnapshot(true));
                renameVerdict.settings_requested = {
                    "name": "Airpods55655567"
                };
                renameVerdict.settings_status = {
                    "name": "confirmed"
                };
                renameVerdict.device.name = "Airpods55655567";
                testWindow.widget.parseState(JSON.stringify(renameVerdict));
                testWindow.check(testWindow.widget.pendingAdvanced.rename === undefined, "a confirmed rename was not retired by the daemon's status");
                testWindow.check(testWindow.widget.advancedCaptions.rename.text === "Confirmed", "a confirmed rename produced no caption");
                testWindow.widget.parseState(testWindow.connectedSnapshot(true));

                Proc.controlled = false;
                if (batteryRight) {
                    const rightTrack = testWindow.findNamed(batteryRight, "batteryTrack");
                    const rightBolt = testWindow.findNamed(batteryRight, "batteryChargingBolt");
                    const rightPercent = testWindow.findNamed(batteryRight, "batteryPercent");
                    const chargingTrackWidth = rightTrack ? rightTrack.width : -1;
                    testWindow.check(rightTrack !== null && rightTrack.width > 0 && rightBolt !== null && rightBolt.visible && rightPercent !== null, "charging row did not expose its bolt, percentage and track");
                    testWindow.check(rightBolt !== null && rightPercent !== null && Math.abs(rightBolt.x + rightBolt.width + 4 - rightPercent.x) < 0.1, "charging bolt is not positioned immediately before the percentage");
                    const notCharging = JSON.parse(testWindow.connectedSnapshot(true));
                    notCharging.battery.right.charging = false;
                    testWindow.widget.parseState(JSON.stringify(notCharging));
                    testWindow.check(!batteryRight.charging && batteryRight.caption === "in case" && !rightBolt.visible, "ending charging did not leave a location-only caption");
                    testWindow.check(Math.abs(rightTrack.width - chargingTrackWidth) < 0.1, "battery track width shifted when the bolt disappeared");
                    notCharging.battery.case.charging = false;
                    testWindow.widget.parseState(JSON.stringify(notCharging));
                    testWindow.check(batteryCase.caption === "lid open", "idle open case did not leave a lid-only caption");
                    notCharging.lid = "closed";
                    testWindow.widget.parseState(JSON.stringify(notCharging));
                    testWindow.check(batteryCase.caption === "", "idle closed case retained a redundant status caption");
                    notCharging.battery.case.source = "ble";
                    testWindow.widget.parseState(JSON.stringify(notCharging));
                    testWindow.check(batteryCase.caption === "BLE observed · audio link not implied", "idle BLE case lost its source or gained an empty separator");
                    const historical = JSON.parse(testWindow.connectedSnapshot(true));
                    historical.battery.right.present = false;
                    historical.battery.right.charging = false;
                    historical.battery.right.last_known_charging = true;
                    historical.battery.right.last_seen = new Date().toISOString();
                    testWindow.widget.parseState(JSON.stringify(historical));
                    testWindow.check(batteryRight.charging && batteryRight.dim && batteryRight.caption.indexOf("last seen charging") === 0 && rightBolt.visible, "historical charging state was not distinguished from a live report");
                    historical.battery.right.last_known_charging = null;
                    testWindow.widget.parseState(JSON.stringify(historical));
                    testWindow.check(!batteryRight.charging && batteryRight.caption.indexOf("charging") < 0, "unknown historical charging state was rendered as known");
                    const historicalIdle = JSON.parse(testWindow.connectedSnapshot(true));
                    for (const side of ["left", "right", "case"]) {
                        historicalIdle.battery[side].present = false;
                        historicalIdle.battery[side].charging = false;
                        historicalIdle.battery[side].last_known_charging = false;
                        historicalIdle.battery[side].last_seen = new Date(Date.now() - 360000).toISOString();
                    }
                    historicalIdle.battery.case.level = 100;
                    testWindow.widget.parseState(JSON.stringify(historicalIdle));
                    for (const side of ["left", "right", "case"])
                        testWindow.check(testWindow.widget.cellCaption(side, 0) === "last seen 6 min ago" && !testWindow.widget.panelCharging(side), "idle history should show only its timestamp: " + side);
                    testWindow.check(batteryRight.dim && !batteryRight.charging && !rightBolt.visible && batteryCase.caption === "last seen 6 min ago", "idle historical row retained a charging indicator or redundant full caption");
                    const staleLegacy = JSON.parse(testWindow.connectedSnapshot(true));
                    staleLegacy.battery.stale = true;
                    staleLegacy.battery.left.charging = true;
                    staleLegacy.battery.right.charging = true;
                    staleLegacy.battery.case.charging = true;
                    staleLegacy.battery.right.last_seen = new Date().toISOString();
                    testWindow.widget.parseState(JSON.stringify(staleLegacy));
                    testWindow.check(testWindow.widget.liveLevel("left") === -1 && testWindow.widget.liveLevel("right") === -1 && testWindow.widget.liveLevel("case") === -1, "stale present cells leaked levels into the bar");
                    testWindow.check(!testWindow.widget.barBudPresent("left") && !testWindow.widget.barBudPresent("right") && testWindow.widget.leftLevel === -1 && testWindow.widget.rightLevel === -1 && testWindow.widget.caseLevel === -1, "stale buds or case remained available to bar icons/readings");
                    testWindow.check(!batteryRight.charging && batteryRight.dim && batteryRight.caption.indexOf("charging") < 0 && !rightBolt.visible, "legacy stale charging=true invented historical charging state");
                    staleLegacy.battery.right.last_known_charging = true;
                    testWindow.widget.parseState(JSON.stringify(staleLegacy));
                    testWindow.check(batteryRight.charging && batteryRight.dim && batteryRight.caption.indexOf("last seen charging") === 0 && rightBolt.visible, "explicit historical charging was not shown as dim panel-only state");
                    testWindow.check(testWindow.widget.rightLevel === -1 && testWindow.widget.caseLevel === -1, "explicit charging history leaked back into the bar");
                    const emptyCells = JSON.parse(testWindow.connectedSnapshot(true));
                    emptyCells.battery.right.level = 0;
                    emptyCells.battery.right.charging = false;
                    emptyCells.battery.case.level = 0;
                    emptyCells.battery.case.charging = false;
                    testWindow.widget.parseState(JSON.stringify(emptyCells));
                    testWindow.check(batteryRight.caption === "in case · case empty", "empty in-case bud state was not explained");
                    testWindow.check(batteryCase.caption === "empty", "empty case state was not explained");
                    const fullCells = JSON.parse(testWindow.connectedSnapshot(true));
                    fullCells.battery.right.level = 100;
                    fullCells.battery.right.charging = false;
                    fullCells.battery.case.level = 100;
                    fullCells.battery.case.charging = false;
                    testWindow.widget.parseState(JSON.stringify(fullCells));
                    testWindow.check(batteryRight.caption === "in case", "full in-case bud should retain only its location caption");
                    testWindow.check(batteryCase.caption === "lid open", "full case should retain only its lid caption");
                    const perCell = JSON.parse(testWindow.connectedSnapshot(true));
                    perCell.battery.right.fresh = false;
                    perCell.battery.right.source = "ble";
                    perCell.battery.right.last_seen = new Date().toISOString();
                    testWindow.widget.parseState(JSON.stringify(perCell));
                    testWindow.check(testWindow.widget.liveLevel("right") === -1 && batteryRight.dim, "an explicitly stale per-cell report was treated as live");
                    perCell.battery.right.fresh = true;
                    testWindow.widget.parseState(JSON.stringify(perCell));
                    testWindow.check(testWindow.widget.liveLevel("right") === 85 && !batteryRight.dim && batteryRight.caption.indexOf("BLE observed · audio link not implied") >= 0, "fresh BLE observation did not retain its non-audio status");
                }
                const nonAdaptive = JSON.parse(testWindow.connectedSnapshot(true));
                nonAdaptive.noise_control = "anc";
                testWindow.widget.parseState(JSON.stringify(nonAdaptive));
                testWindow.check(adaptiveSlider !== null && !adaptiveSlider.enabled && adaptiveControl !== null && !adaptiveControl.visible, "inactive Adaptive adjustment still clutters the quick controls");
                const unlinked = JSON.parse(testWindow.connectedSnapshot(true));
                unlinked.device.aap_link = false;
                testWindow.widget.parseState(JSON.stringify(unlinked));
                testWindow.check(adaptiveSlider !== null && !adaptiveSlider.enabled, "unlinked Adaptive adjustment is still enabled");
                // A daemon healing the link keeps the module on screen with
                // its last readings dimmed and one neutral line. The reason
                // the daemon inferred is never shown.
                const linkRow = testWindow.findNamed(testWindow.panel, "aurisLinkText");
                const linkSlot = testWindow.findNamed(testWindow.panel, "aurisLinkSlot");
                testWindow.check(linkRow !== null && linkSlot !== null, "the link status row is missing from the panel");
                testWindow.check(linkSlot === null || linkSlot.height === 0, "an older daemon without a link object opened the status row");
                testWindow.check(!testWindow.widget.linkReconnecting && testWindow.widget.linkStatusText === "", "a missing link object was read as reconnecting");
                const reconnectCaption = "Attempting to reconnect";
                for (const reason of ["bud_switch", "taken_over", "link_lost", "auto_connect"]) {
                    const healing = JSON.parse(testWindow.connectedSnapshot(true));
                    healing.device.connected = false;
                    healing.device.aap_link = false;
                    healing.link = {
                        "status": "reconnecting",
                        "reason": reason,
                        "attempt": 1,
                        "since": new Date().toISOString()
                    };
                    testWindow.widget.parseState(JSON.stringify(healing));
                    testWindow.check(testWindow.widget.connected && testWindow.widget.moduleShouldShow, reason + ": a reconnect was rendered as a disconnect");
                    testWindow.check(!testWindow.widget.disconnectGrace, reason + ": an announced reconnect fell back to the grace");
                    testWindow.check(testWindow.widget.linkReason === reason, reason + ": the daemon's reason was lost");
                    testWindow.check(testWindow.widget.linkStatusText === reconnectCaption, reason + ": wrong status line: " + testWindow.widget.linkStatusText);
                    testWindow.check(linkRow.text === reconnectCaption, reason + ": the status row did not show the line");
                    testWindow.check(testWindow.widget.barContentOpacity < 1, reason + ": the bar icon was left untouched");
                    testWindow.check(batteryLeft.level === 87 && batteryLeft.dim && batteryRight.dim && batteryCase.dim, reason + ": held readings were dropped or rendered as live");
                }
                const attempts = JSON.parse(testWindow.connectedSnapshot(true));
                attempts.device.connected = false;
                attempts.link = {
                    "status": "reconnecting",
                    "reason": null,
                    "attempt": 2,
                    "since": new Date().toISOString()
                };
                testWindow.widget.parseState(JSON.stringify(attempts));
                testWindow.check(testWindow.widget.linkStatusText === reconnectCaption + " \u00b7 attempt 2", "a retry lost its attempt count: " + testWindow.widget.linkStatusText);
                attempts.link.attempt = 1;
                testWindow.widget.parseState(JSON.stringify(attempts));
                testWindow.check(testWindow.widget.linkStatusText === reconnectCaption, "a first attempt was numbered: " + testWindow.widget.linkStatusText);
                const droppedLink = JSON.parse(testWindow.connectedSnapshot(true));
                droppedLink.device.connected = false;
                droppedLink.device.aap_link = false;
                droppedLink.link = {
                    "status": "disconnected",
                    "reason": "link_lost",
                    "attempt": 3,
                    "since": new Date().toISOString()
                };
                testWindow.widget.parseState(JSON.stringify(droppedLink));
                testWindow.check(!testWindow.widget.connected && testWindow.widget.disconnectGrace && testWindow.widget.moduleShouldShow, "a dropped link hid the module with no grace at all");
                testWindow.check(testWindow.widget.linkStatusText === "", "a finished disconnect kept a reconnecting line on screen");
                // The panel goes with the module, but not before the grace is
                // out and never while the link is being healed. The timed half
                // of this lives in the qmltestrunner regression.
                testWindow.check(testWindow.widget.panelOpen, "the open test panel was not read as open");
                testWindow.check(!testWindow.widget.panelShouldClose && testWindow.widget.closePopoutCount === 0, "the panel was closed inside the disconnect grace");
                testWindow.widget.parseState(testWindow.connectedSnapshot(true));
                testWindow.check(testWindow.widget.connected && !testWindow.widget.disconnectGrace && testWindow.widget.barContentOpacity === 1 && !batteryLeft.dim, "a healed link left the module in its reconnecting treatment");

                testWindow.widget.parseState(testWindow.connectedSnapshot(true));
                for (const label of ["Device name", "Microphone", "Press speed", "Hold duration", "Listening-mode cycle", "Call controls", "Personalized Volume"])
                    testWindow.check(testWindow.containsText(settingsLoader.item, label), "missing full-panel control: " + label);
                if (mainScroll) {
                    testWindow.check(mainScroll.contentHeight <= mainScroll.height + 0.1, "collapsed panel requires scrolling on a normal screen");
                    testWindow.check(quickControls !== null && quickControls.y + quickControls.height <= mainScroll.y, "everyday controls are not fixed above the battery viewport");
                    mainScroll.contentY = 0;
                }
                if (technical) {
                    testWindow.collapsedTechnicalHeight = technical.implicitHeight;
                    testWindow.collapsedPanelHeight = testWindow.panel.implicitHeight;
                    technical.expanded = true;
                }
                testWindow.phase = 1;
                return;
            }
            if (testWindow.phase === 1) {
                const settingsLoader = testWindow.findSettingsLoader(testWindow.panel);
                const technical = testWindow.findNamed(testWindow.panel, "aurisTechnicalDisclosure");
                if (!settingsLoader || !technical || !technical.expanded)
                    return;

                testWindow.check(technical.implicitHeight > testWindow.collapsedTechnicalHeight, "expanding technical details did not grow the fixed footer");
                testWindow.check(testWindow.panel.implicitHeight >= testWindow.collapsedPanelHeight, "technical expansion produced an invalid panel height");
                testWindow.check(testWindow.panel.implicitHeight <= testWindow.panel.verticalBudget + 0.1, "expanded panel exceeded the headless screen-height clamp");
                const quickControls = testWindow.findNamed(testWindow.panel, "aurisQuickControls");
                const setupScroll = testWindow.findNamed(testWindow.panel, "aurisSetupScroll");
                const beforeToastHeight = testWindow.panel.implicitHeight;
                testWindow.widget.showAdvancedToast("warning", "Sent; no device report yet.");
                const toast = testWindow.findNamed(testWindow.panel, "aurisAdvancedToast");
                testWindow.check(toast !== null && toast.visible && toast.height === 52 && testWindow.panel.implicitHeight === beforeToastHeight, "advanced feedback changed panel layout instead of using its fixed slot");
                const toastText = testWindow.findNamed(testWindow.panel, "aurisAdvancedToastText");
                const toastIcon = testWindow.findNamed(testWindow.panel, "aurisAdvancedToastIcon");
                const originalSurfaceText = Theme.surfaceText;
                const originalWarning = Theme.warning;
                const originalError = Theme.error;
                // Both palette directions keep readable surface text over a
                // lightly tinted surface, with a distinct semantic border/icon.
                Theme.surfaceText = "#171717";
                Theme.warning = "#8a5300";
                testWindow.widget.advancedToastText = "";
                testWindow.widget.showAdvancedToast("warning", "Light palette warning");
                testWindow.check(toast.border.color === Theme.warning && toastText.color === Theme.surfaceText && toastIcon.color === Theme.warning, "light palette toast lost semantic contrast roles");
                Theme.surfaceText = "#f7f7f7";
                Theme.error = "#ff8f8f";
                testWindow.widget.advancedToastText = "";
                testWindow.widget.showAdvancedToast("error", "Dark palette error");
                testWindow.check(toast.border.color === Theme.error && toastText.color === Theme.surfaceText && toastIcon.color === Theme.error && Theme.error !== Theme.warning, "dark palette error toast reused warning or low-contrast roles");
                Theme.surfaceText = originalSurfaceText;
                Theme.warning = originalWarning;
                Theme.error = originalError;
                testWindow.check(quickControls !== null && quickControls.visible && quickControls.y === testWindow.quickControlsY, "opening setup moved or hid the fixed quick controls");
                testWindow.check(settingsLoader.visible && setupScroll !== null, "opening the disclosure did not reveal the setup controls");
                if (setupScroll) {
                    setupScroll.contentY = Math.max(0, setupScroll.contentHeight - setupScroll.height);
                    testWindow.check(setupScroll.atYEnd, "device information at the bottom is unreachable");
                    setupScroll.contentY = 0;
                }
                testWindow.widget.parseState(testWindow.connectedSnapshot(false));
                testWindow.check(settingsLoader.item !== null && !settingsLoader.item.available, "old daemon did not disable advanced settings");
                testWindow.check(testWindow.containsText(settingsLoader.item, "Device name") && testWindow.containsText(settingsLoader.item, "Personalized Volume"), "old daemon hid advanced settings instead of disabling them");
                settingsLoader.source = Qt.resolvedUrl("missing/AurisAdvancedSettings.qml");
                testWindow.phase = 2;
                return;
            }
            if (testWindow.phase === 2) {
                const settingsLoader = testWindow.findSettingsLoader(testWindow.panel);
                if (!settingsLoader || settingsLoader.status !== Loader.Error)
                    return;

                testWindow.phase = 3;
                return;
            }
            if (testWindow.phase === 3) {
                const settingsLoader = testWindow.findSettingsLoader(testWindow.panel);
                const errorItem = testWindow.findNamed(testWindow.panel, "aurisSettingsError");
                testWindow.check(errorItem !== null && errorItem.visible && errorItem.height > 0, "advanced component load failure has no visible panel error");
                const retryButton = testWindow.findNamed(testWindow.panel, "aurisSettingsRetry");
                testWindow.check(retryButton !== null, "settings error has no retry action");
                if (!retryButton) {
                    testWindow.finish();
                    return;
                }
                retryButton.clicked();
                testWindow.check(settingsLoader.retrySerial === 1, "retry action did not request a fresh component URL");
                testWindow.phase = 4;
                return;
            }
            if (testWindow.phase === 4) {
                const settingsLoader = testWindow.findSettingsLoader(testWindow.panel);
                if (!settingsLoader || settingsLoader.status !== Loader.Ready || !settingsLoader.item)
                    return;

                testWindow.check(settingsLoader.height > 0, "advanced component did not recover its height after a load retry");
                if (testWindow.captureStarted)
                    return;

                testWindow.captureStarted = true;
                testWindow.phase = 5;
                testWindow.panel.grabToImage(result => {
                    testWindow.check(result.saveToFile("/tmp/auris-panel-offscreen.png"), "could not save the full-panel render artifact");
                    testWindow.widget.parseState(testWindow.connectedSnapshot(true));
                    testWindow.findNamed(testWindow.panel, "aurisTechnicalDisclosure").expanded = false;
                    testWindow.phase = 6;
                });
                return;
            }
            if (testWindow.phase === 6) {
                const help = testWindow.findNamed(testWindow.panel, "aurisAdaptiveHelp");
                testWindow.check(help !== null, "Adaptive quick control lost its explanation");
                if (help) {
                    help.clicked();
                    testWindow.check(help.helpOpen && help.helpPopup.visible, "Adaptive help did not open");
                    help.helpPopup.exit = null;
                    help.helpPopup.close();
                    testWindow.check(!help.helpOpen, "external close left Adaptive help selected");
                    help.clicked();
                    testWindow.check(help.helpOpen && help.helpPopup.visible, "Adaptive help did not reopen in one click");
                    fakePopout.shouldBeVisible = false;
                    testWindow.check(!help.helpOpen, "panel close left Adaptive help open");
                    fakePopout.shouldBeVisible = true;
                }
                testWindow.phase = 7;
                testWindow.panel.grabToImage(result => {
                    testWindow.check(result.saveToFile("/tmp/auris-panel-compact-offscreen.png"), "could not save the collapsed panel preview");
                    testWindow.height = 480;
                    testWindow.findNamed(testWindow.panel, "aurisTechnicalDisclosure").expanded = true;
                    testWindow.widget.parseState(testWindow.connectedSnapshot(false));
                    testWindow.phase = 8;
                });
                return;
            }
            if (testWindow.phase === 8 || testWindow.phase === 9) {
                const quick = testWindow.findNamed(testWindow.panel, "aurisQuickControls");
                const setup = testWindow.findNamed(testWindow.panel, "aurisSetupScroll");
                testWindow.check(testWindow.panel.implicitHeight <= testWindow.panel.verticalBudget + 0.1, "small-screen panel exceeded its usable height at " + testWindow.height);
                testWindow.check(quick.visible && quick.y === testWindow.quickControlsY && quick.y + quick.height <= testWindow.panel.verticalBudget, "quick controls were clipped or moved on a short screen");
                testWindow.check(setup.visible && setup.height >= 64, "setup has no usable viewport on a short screen: " + setup.height);
                setup.contentY = Math.max(0, setup.contentHeight - setup.height);
                testWindow.check(setup.atYEnd, "small-screen setup cannot reach its final controls");
                if (testWindow.phase === 8) {
                    testWindow.height = 420;
                    testWindow.widget.parseState(testWindow.connectedSnapshot(true));
                    testWindow.phase = 9;
                } else {
                    testWindow.finish();
                }
            }
        }
    }
}
