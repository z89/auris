import "../components/settings" as Auris
import QtQuick
import QtQuick.Window
import qs.Common

Window {
    id: testWindow

    property int settingRequests: 0
    property int renameRequests: 0
    property int handoffRequests: 0
    property bool failed: false
    property bool loaderAvailable: false
    property int loaderRequests: 0
    property var loadedSettings: urlLoader.item

    function check(condition, message) {
        if (!condition) {
            console.error("FAIL: " + message);
            failed = true;
        }
    }

    function containsLabel(item, label) {
        if (item.text === label)
            return true;

        return Array.from(item.children || []).some(child => {
            return containsLabel(child, label);
        });
    }

    function findNamed(item, name) {
        if (!item)
            return null;

        if (item.objectName === name)
            return item;

        for (const child of Array.from(item.children || [])) {
            const found = findNamed(child, name);
            if (found)
                return found;
        }
        return null;
    }

    width: 420
    height: 548
    visible: true
    color: "#20242c"

    // Plugin-only reloads must work without a newly indexed sibling QML type.
    Loader {
        id: urlLoader

        visible: false
        width: 420
        Component.onCompleted: setSource(Qt.resolvedUrl("../components/settings/AurisAdvancedSettings.qml") + "?test=inline-panel", {
            "available": Qt.binding(() => {
                return testWindow.loaderAvailable;
            })
        })

        Connections {
            function onSettingRequested(key, value) {
                testWindow.loaderRequests++;
            }

            target: urlLoader.item
        }
    }

    Flickable {
        id: viewport

        anchors.fill: parent
        contentWidth: width
        contentHeight: panel.implicitHeight
        clip: true

        Auris.AurisAdvancedSettings {
            id: panel

            width: viewport.width
            available: false
            unavailableReason: "Update aurisd to use Advanced settings."
            microphone: null
            pressSpeed: null
            holdDuration: null
            listeningModeCycle: null
            callControls: null
            personalizedVolume: null
            onRenameRequested: testWindow.renameRequests++
            onSettingRequested: testWindow.settingRequests++
            onHandoffToggleRequested: testWindow.handoffRequests++
        }
    }

    Timer {
        interval: 50
        running: true
        repeat: false
        onTriggered: {
            testWindow.check(urlLoader.status === Loader.Ready, "URL-loaded settings failed to instantiate");
            const loadedPanel = testWindow.loadedSettings;
            testWindow.check(loadedPanel !== null, "URL loader returned the wrong component");
            if (loadedPanel) {
                testWindow.check(!loadedPanel.available, "URL loader lost initial binding");
                testWindow.loaderAvailable = true;
                testWindow.check(loadedPanel.available, "URL loader binding did not update");
                testWindow.check(urlLoader.height === loadedPanel.implicitHeight, "URL loader lost content height");
                loadedPanel.submitSetting("microphone", "left");
                testWindow.check(testWindow.loaderRequests === 1, "URL loader signal was not forwarded");
            }
            // All settings share the outer panel's bounded scroll area.
            testWindow.check(viewport.height === 548, "outer viewport is unbounded");
            testWindow.check(panel.height === panel.implicitHeight && panel.height > viewport.height, "settings content was truncated");
            for (const label of ["Device name", "Microphone", "Press speed", "Hold duration", "Listening-mode cycle", "Call controls", "Personalized Volume"])
                testWindow.check(testWindow.containsLabel(panel, label), "missing inline control: " + label);
            const settingsSurface = testWindow.findNamed(panel, "advancedSettingsSurface");
            // Every setting now reserves a caption lane for the daemon's own
            // verification verdict, which a control may never be without.
            testWindow.check(settingsSurface !== null && settingsSurface.height <= 760, "wrapped compact settings surface grew beyond 760 px: " + (settingsSurface ? settingsSurface.height : "missing"));
            const helpQualifications = {
                "rename": "Apple devices keep their own name",
                "microphone": "Bluetooth microphone codec",
                "press_speed": "does not change what a press does",
                "hold_duration": "does not change the action",
                "listening_cycle": "does not promise a custom order",
                "call_controls": "cannot be changed independently",
                "personalized_volume": "Linux is not guaranteed"
            };
            for (const key of Object.keys(helpQualifications)) {
                const info = testWindow.findNamed(panel, "settingsHelp_" + key);
                testWindow.check(info !== null, "missing info affordance: " + key);
                testWindow.check(info !== null && info.helpText.indexOf(helpQualifications[key]) >= 0 && info.tooltipText === null, "missing qualification or duplicate hover tooltip: " + key);
            }
            const microphoneHelp = testWindow.findNamed(panel, "settingsHelp_microphone");
            const pressHelp = testWindow.findNamed(panel, "settingsHelp_press_speed");
            testWindow.check(panel.activeHelpKey === "" && microphoneHelp && !microphoneHelp.helpOpen, "a setting explanation started open");
            if (microphoneHelp) {
                testWindow.check(microphoneHelp.tooltipText === null, "closed info button enables a second tooltip owner");
                microphoneHelp.clicked();
                testWindow.check(panel.activeHelpKey === "microphone" && microphoneHelp.helpOpen && microphoneHelp.tooltipVisible && microphoneHelp.tooltipText === null, "click did not open the microphone explanation");
                testWindow.check(microphoneHelp.helpText.indexOf("Bluetooth microphone codec") >= 0, "microphone explanation lost its technical qualification");
            }
            if (pressHelp) {
                pressHelp.clicked();
                testWindow.check(panel.activeHelpKey === "press_speed" && pressHelp.helpOpen && pressHelp.tooltipVisible && !microphoneHelp.helpOpen && !microphoneHelp.tooltipVisible, "click did not replace the open explanation");
                pressHelp.clicked();
                testWindow.check(panel.activeHelpKey === "" && !pressHelp.helpOpen, "second click did not close the explanation");
            }
            if (microphoneHelp) {
                microphoneHelp.clicked();
                microphoneHelp.helpPopup.exit = null;
                microphoneHelp.helpPopup.close();
                testWindow.check(!microphoneHelp.helpOpen && panel.activeHelpKey === "" && microphoneHelp.tooltipText === null, "external tooltip close left its owner selected");
                microphoneHelp.clicked();
                testWindow.check(microphoneHelp.helpOpen && microphoneHelp.tooltipVisible, "one click did not reopen an externally closed explanation");
                panel.visible = false;
                testWindow.check(panel.activeHelpKey === "" && !microphoneHelp.helpOpen && !microphoneHelp.tooltipVisible, "collapsing the settings component left a click tooltip open");
                panel.visible = true;
            }
            viewport.contentY = viewport.contentHeight - viewport.height;
            testWindow.check(viewport.contentY > 0 && viewport.atYEnd, "bottom controls cannot be reached by scrolling");
            viewport.contentY = 0;
            testWindow.check(!panel.submitSetting("microphone", "left"), "unavailable setting was submitted");
            testWindow.check(testWindow.settingRequests === 0, "unavailable setting emitted a request");
            // Unknown confirmed values remain null but do not prevent an explicit choice.
            panel.available = true;
            panel.unavailableReason = "";
            testWindow.check(panel.microphone === null && panel.personalizedVolume === null, "unknown became a concrete value");
            const microphoneRow = testWindow.findNamed(panel, "settingRow_microphone");
            testWindow.check(microphoneRow !== null && !microphoneRow.confirmedKnown && testWindow.containsLabel(microphoneRow, "Not reported"), "unknown microphone state was not kept visible");
            panel.microphone = "unexpected";
            testWindow.check(!microphoneRow.confirmedKnown && testWindow.containsLabel(microphoneRow, "Not reported"), "unexpected microphone state was presented as confirmed");
            panel.microphone = null;
            testWindow.check(panel.submitSetting("microphone", "right"), "explicit unknown setting was rejected");
            testWindow.check(testWindow.settingRequests === 1, "explicit setting signal missing");
            // Pending feedback is local to its setting and never blocks others.
            const stableSettingsHeight = panel.implicitHeight;
            const stableSurfaceHeight = settingsSurface.height;
            panel.pendingRequests = {
                "microphone": {
                    "target": "right",
                    "state": "awaiting"
                }
            };
            panel.microphone = "auto";
            const requestedMic = testWindow.findNamed(panel, "settingChoice_microphone_2");
            const confirmedMic = testWindow.findNamed(panel, "settingChoice_microphone_0");
            testWindow.check(microphoneRow.requestText === "Waiting\u2026" && testWindow.containsLabel(microphoneRow, "Waiting\u2026"), "pending microphone request has no fixed-lane indicator");
            testWindow.check(panel.implicitHeight === stableSettingsHeight && settingsSurface.height === stableSurfaceHeight, "pending status changed settings geometry");
            testWindow.check(requestedMic !== null && requestedMic.selected && requestedMic.requestedSelected && !requestedMic.confirmedSelected, "requested target was not selected independently from the confirmed value");
            testWindow.check(confirmedMic !== null && confirmedMic.confirmedSelected && !confirmedMic.selected, "confirmed value was overwritten instead of retained separately");
            testWindow.check(panel.submitSetting("press_speed", "slower"), "independent setting was blocked by pending microphone");
            testWindow.check(panel.submitRename("Office Pods"), "rename was blocked by pending microphone");
            testWindow.check(testWindow.settingRequests === 2 && testWindow.renameRequests === 1, "independent pending requests did not emit signals");
            const unselectedMic = confirmedMic;
            Theme.buttonBg = Theme.primary;
            panel.microphone = "right";
            testWindow.check(unselectedMic !== null && unselectedMic.backgroundColor !== Theme.primary && unselectedMic.textColor === Theme.surfaceText, "unselected choice reused the primary button palette");
            // UTF-8 length, controls, blank names, and invalid surrogate handling.
            testWindow.check(panel.renameValidationError("   ").length > 0, "blank rename accepted");
            testWindow.check(panel.renameValidationError("Pods\nDesk").length > 0, "control character accepted");
            testWindow.check(panel.renameValidationError("€".repeat(85)) === "", "255-byte UTF-8 name rejected");
            testWindow.check(panel.renameValidationError("€".repeat(86)).length > 0, "overlong UTF-8 name accepted");
            testWindow.check(panel.renameValidationError(String.fromCharCode(55296)).length > 0, "invalid Unicode accepted");
            // Cycle values are canonical, unique, and require two entries in the UI.
            const normalized = panel.normalizedCycle(["adaptive", "anc", "anc", "bogus", "off"]);
            testWindow.check(JSON.stringify(normalized) === '["off","anc","adaptive"]', "cycle normalization failed");
            panel.cycleDraft = ["anc"];
            panel.cycleEdited = true;
            testWindow.check(panel.normalizedCycle(panel.cycleDraft).length < 2, "single-mode cycle became valid");
            panel.toggleCycleMode("off");
            testWindow.check(JSON.stringify(panel.cycleDraft) === '["off","anc"]', "cycle toggle/order failed");
            testWindow.check(!panel.toggleCycleMode("off") && JSON.stringify(panel.cycleDraft) === '["off","anc"]' && panel.cycleNotice.indexOf("at least two") >= 0, "valid listening cycle was allowed to diverge to one unsent mode");
            const sentBeforeCycle = testWindow.settingRequests;
            const cycleTransparency = testWindow.findNamed(panel, "settingChoice_listening_cycle_2");
            if (cycleTransparency)
                cycleTransparency.clicked();

            testWindow.check(testWindow.settingRequests === sentBeforeCycle + 1, "a valid listening-mode change did not send immediately");
            panel.listeningModeCycle = ["transparency", "adaptive"];
            testWindow.check(JSON.stringify(panel.cycleDraft) === '["off","anc","transparency"]', "external report overwrote a deliberate draft");
            panel.deviceIdentity = "AA:BB:CC:DD:EE:FF";
            testWindow.check(JSON.stringify(panel.cycleDraft) === '["transparency","adaptive"]', "device change retained another device's draft");
            // Seamless switching is inert until the daemon publishes `handoff`.
            const handoffToggle = testWindow.findNamed(panel, "aurisHandoffToggle");
            const appleIdCaption = testWindow.findNamed(panel, "aurisHandoffAppleIdCaption");
            testWindow.check(handoffToggle !== null && appleIdCaption !== null, "seamless switching controls missing");
            if (handoffToggle && appleIdCaption) {
                testWindow.check(!handoffToggle.enabled && !appleIdCaption.visible, "seamless switching usable without a handoff object");
                testWindow.check(!panel.submitHandoff(true) && testWindow.handoffRequests === 0, "handoff request sent without a handoff object");
                panel.handoff = {
                    "enabled": false,
                    "take_over_on_play": true,
                    "apple_host_id": false,
                    "owner": "unknown",
                    "audio_source": null,
                    "devices": [],
                    "last_event": null
                };
                testWindow.check(handoffToggle.enabled && !handoffToggle.checked && appleIdCaption.visible, "handoff disabled state or Apple ID caption wrong");
                handoffToggle.toggled(true);
                testWindow.check(testWindow.handoffRequests === 1, "handoff toggle request not emitted");
                panel.handoff = Object.assign({}, panel.handoff, {
                    "enabled": true,
                    "apple_host_id": true
                });
                testWindow.check(handoffToggle.checked && !appleIdCaption.visible, "handoff enabled state or Apple ID caption wrong");
                panel.handoff = null;
            }
            if (testWindow.failed) {
                Qt.exit(1);
                return;
            }
            // Render the actual component and retain a reviewable offscreen artifact.
            panel.grabToImage(result => {
                if (!result.saveToFile("/tmp/auris-advanced-offscreen.png")) {
                    console.error("FAIL: could not save render artifact");
                    Qt.exit(1);
                    return;
                }
                Qt.quit();
            });
        }
    }

    Timer {
        interval: 3000
        running: true
        repeat: false
        onTriggered: {
            console.error("FAIL: offscreen component test timed out");
            Qt.exit(1);
        }
    }
}
