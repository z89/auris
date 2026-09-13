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
            snapshot.settings_api = 1;
            snapshot.settings = {
                "microphone": "auto",
                "press_speed": "default",
                "hold_duration": "default",
                "listening_mode_cycle": ["anc", "transparency", "adaptive"],
                "call_controls": "mute_once_hangup_twice",
                "personalized_volume": true
            };
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

                // A repeated snapshot is not an echo of a write. New daemons
                // provide a per-key sequence so an old Auto report cannot
                // confirm a later request until its counter advances.
                const sequenced = JSON.parse(testWindow.connectedSnapshot(true));
                sequenced.settings_report_seq = {
                    "microphone": 7
                };
                testWindow.check(testWindow.widget.settingsReportSequenceFor(sequenced, "press_speed") === 0, "missing per-key report counter did not use baseline zero");
                testWindow.widget.parseState(JSON.stringify(sequenced));
                testWindow.widget.pendingAdvanced = {
                    "microphone": {
                        "target": "right",
                        "address": sequenced.device.address,
                        "state": "awaiting",
                        "queued": null,
                        "reportSequence": 7
                    }
                };
                const staleMatchingCounter = JSON.parse(JSON.stringify(sequenced));
                staleMatchingCounter.settings.microphone = "right";
                testWindow.widget.parseState(JSON.stringify(staleMatchingCounter));
                testWindow.check(testWindow.widget.pendingAdvanced.microphone !== undefined, "unchanged setting report counter falsely confirmed a write");
                staleMatchingCounter.settings_report_seq.microphone = 8;
                testWindow.widget.parseState(JSON.stringify(staleMatchingCounter));
                testWindow.check(testWindow.widget.pendingAdvanced.microphone === undefined, "new matching setting report did not confirm the write");
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
