import QtQuick
import QtQuick.Window
import QtTest
import qs.Common
import qs.Services
import qs.DankCommon.Common as Dms

Rectangle {
    id: scene
    width: 1100
    height: 1000
    color: Theme.surfaceContainerHigh
    property bool backgroundUpdatesRequested: false

    QtObject {
        id: fakeScreen
        property real width: 1100
        property real height: 1000
    }
    QtObject {
        id: testContentWindow
        property bool updatesEnabled: true
    }
    QtObject {
        id: testBackgroundWindow
        property bool updatesEnabled: scene.backgroundUpdatesRequested
    }
    QtObject {
        id: host
        property bool fullHeightSurface: false
        property bool shouldBeVisible: true
        property bool backgroundInteractive: true
        property bool useConnectedBackend: false
        property bool isClosing: false
        property real alignedX: 10
        property real alignedY: 20
        property real alignedWidth: 420
        property real alignedHeight: renderedAlignedHeight
        property real maskX: 0
        property real maskY: 0
        property real maskWidth: scene.width
        property real maskHeight: scene.height
        property var contentWindow: testContentWindow
        property var backgroundWindow: testBackgroundWindow
        property real renderedAlignedHeight: panel.item ? panel.item.implicitHeight + Theme.spacingS * 2 : 0
        Behavior on renderedAlignedHeight {
            NumberAnimation {
                duration: 180
                easing.type: Easing.InOutQuad
            }
        }
    }
    Loader {
        id: widget
        source: "../AurisWidget.qml"
        onLoaded: {
            item.parentScreen = fakeScreen;
            item.parseState(JSON.stringify({
                schema: 1,
                updated_at: new Date().toISOString(),
                daemon: {
                    version: "test",
                    source: "aap"
                },
                settings_api: 2,
                device: {
                    address: "test",
                    name: "AirPods",
                    model_id: "201B",
                    model: "AirPods 4 (ANC)",
                    connected: true,
                    aap_link: true
                },
                battery: {
                    stale: false,
                    left: {
                        level: 85,
                        present: true
                    },
                    right: {
                        level: 90,
                        present: true
                    },
                    case: {
                        level: 75,
                        present: true
                    }
                },
                ear: {
                    left: "in",
                    right: "in"
                },
                noise_control: "adaptive",
                adaptive_level: 50,
                conversational_awareness: true,
                settings: {
                    microphone: "auto",
                    press_speed: "default",
                    hold_duration: "default",
                    listening_mode_cycle: ["anc", "transparency"],
                    call_controls: "mute_once_hangup_twice",
                    personalized_volume: true
                },
                settings_requested: {},
                settings_status: {},
                settings_verify: "idle",
                verify_reopen: false
            }));
            panel.sourceComponent = item.popoutContent;
        }
    }
    Loader {
        id: panel
        width: widget.item ? widget.item.popoutWidth - Theme.spacingS * 2 : 420
        onLoaded: item.parentPopout = host
    }

    TestCase {
        name: "AurisRealWidgets"
        when: windowShown

        function node(name) {
            return findChild(panel.item, name);
        }
        function settings() {
            return node("aurisSettingsLoader").item;
        }
        function settle() {
            wait(260);
        }
        function childrenWithText(item, text) {
            let result = [];
            for (const child of item.children || []) {
                if (child.text === text && typeof child.implicitWidth === "number")
                    result.push(child);
                result = result.concat(childrenWithText(child, text));
            }
            return result;
        }
        function checkButton(button) {
            verify(button !== null);
            const labels = childrenWithText(button, button.text);
            verify(labels.length > 0, "No real DMS label in " + button.objectName);
            for (const label of labels)
                verify(label.implicitWidth + button.horizontalPadding * 2 <= button.width + 0.1, button.objectName + " overflows: label " + label.implicitWidth + " + padding " + button.horizontalPadding * 2 + " > " + button.width);
            verify(button.x >= 0 && button.x + button.width <= button.parent.width + 0.1);
        }
        function initTestCase() {
            failOnWarning(/.*/);
            scene.Window.window.width = scene.width;
            scene.Window.window.height = scene.height;
            tryCompare(widget, "status", Loader.Ready);
            tryCompare(panel, "status", Loader.Ready);
            tryVerify(() => node("aurisSettingsLoader")?.status === Loader.Ready);
            settle();
        }
        function cleanup() {
            Theme.fontSizeMedium = 14;
            Dms.Style.fontSizeMedium = 14;
            fakeScreen.width = 1100;
            settings().activeHelpKey = "";
            node("aurisTechnicalDisclosure").expanded = false;
            settle();
        }

        function test_00_model_header_and_right_aligned_status() {
            const header = node("aurisPanelHeader");
            const title = node("aurisPanelTitle");
            const status = node("aurisPanelStatus");
            const close = node("aurisPanelClose");
            const original = JSON.stringify(widget.item.st);
            const quickY = node("aurisQuickControls").y;
            try {
                compare(header.height, 40);
                compare(title.text, widget.item.model);
                compare(title.font.pixelSize, Theme.fontSizeXLarge - 2);
                verify(title.implicitWidth <= title.width, "Reported model is truncated at normal panel width");
                for (const screenWidth of [1100, 480, 360]) {
                    fakeScreen.width = screenWidth;
                    for (const state of ["off", "anc", "transparency", "adaptive", "unknown", "disconnected", "unavailable", "long-model", "missing-model"]) {
                        const snapshot = JSON.parse(original);
                        snapshot.noise_control = state;
                        snapshot.device.connected = state !== "disconnected";
                        if (state === "long-model")
                            snapshot.device.model = "An unusually long reported AirPods model name";
                        if (state === "missing-model")
                            snapshot.device.model = "";
                        widget.item.parseState(JSON.stringify(snapshot));
                        if (state === "unavailable")
                            widget.item.daemonUp = false;
                        wait(30);
                        compare(title.text, snapshot.device.model || snapshot.device.name);
                        compare(status.text, widget.item.headerStatus);
                        compare(status.horizontalAlignment, Text.AlignRight);
                        verify(title.x + title.width + header.spacing <= status.x + 1, "Header labels overlap or lose their gutter");
                        // Qt rounds layout positions to pixels while text's
                        // preferred width retains fractional glyph advances.
                        verify(Math.abs(status.x + status.width + header.spacing - close.x) < 1, "Status is not aligned to the close-button gutter");
                        verify(close.x + close.width <= header.width + 0.1, "Close button overflows header");
                        verify(Math.abs(title.y + title.height / 2 - header.height / 2) <= 0.51, "Model is not vertically centered");
                        verify(Math.abs(status.y + status.height / 2 - header.height / 2) <= 0.51, "Status is not vertically centered");
                        compare(header.height, 40);
                        compare(node("aurisQuickControls").y, quickY);
                    }
                }
            } finally {
                widget.item.parseState(original);
                fakeScreen.width = 1100;
                settle();
            }
        }

        function test_01_height_reveal_keeps_quick_controls_fixed() {
            verify(host.fullHeightSurface, "Auris did not enable DMS's stable surface");
            verify(panel.item.clip, "Revealed content is not clipped to host height");
            const quick = node("aurisQuickControls");
            const initial = quick.mapToItem(scene, 0, 0);
            const before = grabImage(quick);
            const width = quick.width;
            const bodyWidth = widget.item.popoutWidth;
            for (const expanding of [true, false, true, false]) {
                let intermediate = 0;
                let previous = panel.item.height;
                node("aurisTechnicalDisclosure").expanded = expanding;
                for (let frame = 0; frame < 15; frame++) {
                    wait(16);
                    const position = quick.mapToItem(scene, 0, 0);
                    compare(position.x, initial.x);
                    compare(position.y, initial.y);
                    compare(quick.width, width);
                    compare(widget.item.popoutWidth, bodyWidth);
                    compare(quick.scale, 1);
                    verify(Math.abs(panel.item.height + Theme.spacingS * 2 - host.renderedAlignedHeight) < 0.1);
                    verify(expanding ? panel.item.height >= previous - 0.1 : panel.item.height <= previous + 0.1, "Height reversed/overshot during reveal");
                    if (Math.abs(panel.item.height - previous) > 0.1)
                        intermediate++;
                    previous = panel.item.height;
                    verify(before.equals(grabImage(quick)), "Quick controls changed pixels during disclosure animation");
                    verify(testBackgroundWindow.updatesEnabled, "Dismiss input updates were suspended during resize");
                }
                verify(intermediate > 2, "Disclosure snapped instead of animating");
            }
        }

        function test_01b_input_updates_restore_original_binding() {
            try {
                compare(testBackgroundWindow.updatesEnabled, true);
                host.isClosing = true;
                host.shouldBeVisible = false;
                compare(testBackgroundWindow.updatesEnabled, true);
                host.isClosing = false;
                compare(testBackgroundWindow.updatesEnabled, false);
                scene.backgroundUpdatesRequested = true;
                compare(testBackgroundWindow.updatesEnabled, true);
                scene.backgroundUpdatesRequested = false;
                compare(testBackgroundWindow.updatesEnabled, false);
                host.shouldBeVisible = true;
                compare(testBackgroundWindow.updatesEnabled, true);
                // A connected backend shares one surface; never override it.
                testContentWindow.updatesEnabled = false;
                host.backgroundWindow = testContentWindow;
                compare(testBackgroundWindow.updatesEnabled, false);
                compare(testContentWindow.updatesEnabled, false);
                host.backgroundWindow = testBackgroundWindow;
                compare(testBackgroundWindow.updatesEnabled, true);
            } finally {
                scene.backgroundUpdatesRequested = false;
                testContentWindow.updatesEnabled = true;
                host.backgroundWindow = testBackgroundWindow;
                host.isClosing = false;
                host.shouldBeVisible = true;
            }
        }

        function test_01c_expanded_setup_scroll_receives_wheel() {
            node("aurisTechnicalDisclosure").expanded = true;
            settle();
            const scroll = node("aurisSetupScroll");
            scroll.contentY = 0;
            verify(scroll.contentHeight > scroll.height);
            mouseWheel(scroll, scroll.width / 2, scroll.height / 2, 0, -120);
            tryVerify(() => scroll.contentY > 0);
            scroll.contentY = 0;
            node("aurisTechnicalDisclosure").expanded = false;
            settle();
        }

        function test_02_settings_fit_narrow_and_large_font_layouts() {
            node("aurisTechnicalDisclosure").expanded = true;
            const normalPreferredWidth = widget.item.preferredPanelWidth;
            for (const fontSize of [14, 19]) {
                Theme.fontSizeMedium = fontSize;
                Dms.Style.fontSizeMedium = fontSize;
                settle();
                if (fontSize === 19)
                    verify(widget.item.preferredPanelWidth > normalPreferredWidth, "Panel measurement did not react to a font-only change");
                for (const screenWidth of [1100, 480, 360]) {
                    fakeScreen.width = screenWidth;
                    settle();
                    verify(widget.item.popoutWidth <= screenWidth - 32);
                    for (const spec of [["microphone", 3], ["press_speed", 3], ["hold_duration", 3], ["call_controls", 2], ["personalized_volume", 2], ["listening_cycle", 4]]) {
                        for (let i = 0; i < spec[1]; i++)
                            checkButton(node("settingChoice_" + spec[0] + "_" + i));
                    }
                    const surface = node("advancedSettingsSurface");
                    const grid = node("settingChoice_microphone_0").parent;
                    const left = grid.mapToItem(surface, 0, 0).x;
                    verify(Math.abs(left - (surface.width - left - grid.width)) < 0.1, "Setup card has asymmetric gutters");
                    const quick = node("aurisQuickControls");
                    const main = node("aurisMainScroll");
                    const setup = node("aurisSetupScroll");
                    compare(quick.x, main.x);
                    compare(main.x, node("aurisTechnicalDisclosure").x);
                    compare(quick.width, main.width);
                    compare(main.width, setup.width);
                }
            }
        }

        function test_03_one_tooltip_after_hover_click_escape_and_outside_click() {
            node("aurisTechnicalDisclosure").expanded = true;
            node("aurisSetupScroll").contentY = 0;
            settle();
            const info = node("settingsHelp_microphone");
            const nativeTooltip = findChild(info, "realDmsHoverTooltip");
            verify(nativeTooltip !== null, "Real DMS hover tooltip code is not under test");
            mouseMove(info, 12, 12);
            wait(450);
            verify(!nativeTooltip.visible, "Hover opened a competing DMS tooltip");
            mouseClick(info, 12, 12);
            tryCompare(info.helpPopup, "visible", true);
            wait(450);
            verify(!nativeTooltip.visible, "Hover timer opened a second tooltip after click");
            keyClick(Qt.Key_Escape);
            tryCompare(info, "helpOpen", false);
            mouseClick(info, 12, 12);
            tryCompare(info.helpPopup, "visible", true);
            mouseClick(scene, 1000, 950);
            tryCompare(info, "helpOpen", false);
            verify(!nativeTooltip.visible);
            mouseClick(info, 12, 12);
            tryCompare(info.helpPopup, "visible", true);
            node("aurisTechnicalDisclosure").expanded = false;
            tryCompare(info, "helpOpen", false);
            settle();
            const adaptive = node("aurisAdaptiveHelp");
            const adaptiveNative = findChild(adaptive, "realDmsHoverTooltip");
            verify(adaptiveNative !== null);
            mouseMove(adaptive, 12, 12);
            wait(450);
            verify(!adaptiveNative.visible);
            mouseClick(adaptive, 12, 12);
            tryCompare(adaptive.helpPopup, "visible", true);
            wait(450);
            verify(!adaptiveNative.visible, "Adaptive help opened two tooltips");
            keyClick(Qt.Key_Escape);
            tryCompare(adaptive, "helpOpen", false);
        }

        function test_03b_real_setting_click_reaches_command_and_report() {
            const original = JSON.stringify(widget.item.st);
            const snapshot = JSON.parse(original);
            snapshot.settings_requested = {};
            snapshot.settings_status = {};
            Proc.controlled = true;
            Proc.reset();
            try {
                widget.item.parseState(JSON.stringify(snapshot));
                node("aurisTechnicalDisclosure").expanded = true;
                settle();
                const scroll = node("aurisSetupScroll");
                const row = node("settingRow_press_speed");
                scroll.contentY = Math.min(scroll.contentHeight - scroll.height, row.mapToItem(settings(), 0, 0).y);
                settle();
                const fixedY = node("aurisQuickControls").y;
                for (const choice of [[1, "slower"], [2, "slowest"], [0, "default"]]) {
                    widget.item.dismissAdvancedToast();
                    const button = node("settingChoice_press_speed_" + choice[0]);
                    const previousCalls = Proc.calls.length;
                    mouseClick(button);
                    tryVerify(() => Proc.calls.length === previousCalls + 1, 1000, "A real settings click never reached the command runner");
                    const call = Proc.calls[previousCalls];
                    compare(call.argv.slice(-3).join("|"), "setting|press_speed|" + JSON.stringify(choice[1]));
                    compare(widget.item.confirmedSetting("press_speed"), snapshot.settings.press_speed, "Click invented a device confirmation");
                    Proc.complete(previousCalls, '{"ok":true}', 0);
                    compare(widget.item.pendingAdvancedFor("press_speed").state, "awaiting");
                    compare(widget.item.pendingAdvancedFor("press_speed").inFlightSerial, 0, "Completed command would block another click");
                    // The daemon takes the request over: it appears in
                    // settings_requested under a status of its own, and the
                    // local entry retires. No timer decides anything.
                    snapshot.settings_requested.press_speed = choice[1];
                    snapshot.settings_status.press_speed = "verifying";
                    widget.item.parseState(JSON.stringify(snapshot));
                    compare(widget.item.pendingAdvancedFor("press_speed"), null);
                    compare(widget.item.effectiveSetting("press_speed"), choice[1], "the control snapped back while the daemon was verifying");
                    compare(widget.item.advancedCaptions.press_speed.text, "Verifying with AirPods\u2026");
                    snapshot.settings.press_speed = choice[1];
                    snapshot.settings_status.press_speed = "confirmed";
                    widget.item.parseState(JSON.stringify(snapshot));
                    compare(widget.item.confirmedSetting("press_speed"), choice[1]);
                    compare(widget.item.advancedCaptions.press_speed.text, "Confirmed");
                    compare(node("aurisQuickControls").y, fixedY);
                }
            } finally {
                widget.item.dismissAdvancedToast();
                widget.item.parseState(original);
                Proc.controlled = false;
                Proc.reset();
            }
        }

        function test_04_real_widget_preview() {
            node("aurisTechnicalDisclosure").expanded = true;
            settle();
            grabImage(panel.item).save("/tmp/auris-real-widgets.png");
            const scroll = node("aurisSetupScroll");
            const call = node("settingRow_call_controls");
            scroll.contentY = Math.min(scroll.contentHeight - scroll.height, call.mapToItem(settings(), 0, 0).y);
            settle();
            grabImage(panel.item).save("/tmp/auris-real-widgets-bottom.png");
        }

        function test_03c_verification_reopen_is_not_a_disconnect() {
            const original = JSON.stringify(widget.item.st);
            const snapshot = JSON.parse(original);
            snapshot.settings.press_speed = "default";
            Proc.controlled = true;
            Proc.reset();
            try {
                widget.item.parseState(JSON.stringify(snapshot));
                node("aurisTechnicalDisclosure").expanded = true;
                settle();
                const stableGeometry = {
                    "panelHeight": panel.item.implicitHeight,
                    "batteryContentHeight": node("aurisMainScroll").contentHeight,
                    "batteryLeftHeight": node("batteryLeft").height,
                    "batteryRightHeight": node("batteryRight").height,
                    "batteryCaseHeight": node("batteryCase").height,
                    "setupY": node("aurisTechnicalDisclosure").mapToItem(scene, 0, 0).y,
                    "settingsHeight": settings().implicitHeight
                };
                function verifyStableGeometry(label) {
                    compare(panel.item.implicitHeight, stableGeometry.panelHeight, label + ": panel height changed");
                    compare(node("aurisMainScroll").contentHeight, stableGeometry.batteryContentHeight, label + ": battery content height changed");
                    compare(node("batteryLeft").height, stableGeometry.batteryLeftHeight, label + ": left battery row changed");
                    compare(node("batteryRight").height, stableGeometry.batteryRightHeight, label + ": right battery row changed");
                    compare(node("batteryCase").height, stableGeometry.batteryCaseHeight, label + ": case battery row changed");
                    compare(node("aurisTechnicalDisclosure").mapToItem(scene, 0, 0).y, stableGeometry.setupY, label + ": setup moved");
                    compare(settings().implicitHeight, stableGeometry.settingsHeight, label + ": a caption moved the settings rows");
                }
                mouseClick(node("settingChoice_press_speed_1"));
                tryVerify(() => Proc.calls.length === 1);
                Proc.complete(0, '{"ok":true}', 0);
                compare(Proc.calls.length, 1, "the widget reopened the link itself instead of leaving readback to the daemon");
                compare(widget.item.pendingAdvancedFor("press_speed").state, "awaiting");

                // The daemon drops and reopens the AAP link purely to read the
                // write back, reporting connected and aap_link false while it
                // does. Nothing on screen may call that a disconnect.
                const reopening = JSON.parse(JSON.stringify(snapshot));
                reopening.settings_requested = {
                    "press_speed": "slower"
                };
                reopening.settings_status = {
                    "press_speed": "verifying"
                };
                reopening.settings_verify = "reopening";
                reopening.verify_reopen = true;
                reopening.device.connected = false;
                reopening.device.aap_link = false;
                reopening.battery.stale = true;
                for (const side of ["left", "right", "case"])
                    reopening.battery[side].fresh = false;
                widget.item.parseState(JSON.stringify(reopening));
                wait(20);
                verify(widget.item.connected, "a verification reopen was rendered as a disconnect");
                verify(!widget.item.deviceConnected, "the raw device connection state was lost");
                verify(widget.item.wantVisible, "the bar pill dropped out for a verification reopen");
                verify(widget.item.advancedAvailable, "the settings controls were disabled by a verification reopen");
                verify(!widget.item.stale, "a verification reopen dimmed the panel");
                compare(widget.item.pendingAdvancedFor("press_speed"), null, "the daemon took the request over but the local entry stayed");
                compare(widget.item.effectiveSetting("press_speed"), "slower", "the control snapped back during the reopen");
                compare(node("batteryLeft").caption, "", "a verification reopen aged the battery rows");
                compare(settings().captionFor("press_speed"), "Verifying with AirPods\u2026");
                verifyStableGeometry("verification reopen");

                const settled = JSON.parse(JSON.stringify(reopening));
                settled.device.connected = true;
                settled.device.aap_link = true;
                settled.verify_reopen = false;
                settled.battery.stale = false;
                settled.settings.press_speed = "slower";
                settled.settings_status.press_speed = "confirmed";
                settled.settings_verify = "idle";
                widget.item.parseState(JSON.stringify(settled));
                wait(20);
                compare(widget.item.confirmedSetting("press_speed"), "slower");
                compare(settings().captionFor("press_speed"), "Confirmed");
                verifyStableGeometry("verification confirmed");

                const kept = JSON.parse(JSON.stringify(settled));
                kept.settings.press_speed = "default";
                kept.settings_status.press_speed = "mismatch";
                widget.item.parseState(JSON.stringify(kept));
                wait(20);
                compare(widget.item.effectiveSetting("press_speed"), "default", "a mismatch did not return the control to the value the AirPods kept");
                compare(settings().captionFor("press_speed"), "AirPods kept Default");
                compare(String(settings().captionColorFor("press_speed")), String(Theme.error), "a mismatch is not in the error colour");
                verifyStableGeometry("AirPods kept their own value");

                const unreported = JSON.parse(JSON.stringify(kept));
                unreported.settings_status.press_speed = "unreported";
                widget.item.parseState(JSON.stringify(unreported));
                wait(20);
                compare(widget.item.effectiveSetting("press_speed"), "slower", "an unreported write was withdrawn from the control");
                compare(settings().captionFor("press_speed"), "Applied. AirPods 4 doesn't report this setting back.");
                verifyStableGeometry("AirPods never report this setting");

                const unverified = JSON.parse(JSON.stringify(unreported));
                unverified.settings_status.press_speed = "unverified";
                widget.item.parseState(JSON.stringify(unverified));
                wait(20);
                compare(settings().captionFor("press_speed"), "Sent, not verified");
                compare(String(settings().captionColorFor("press_speed")), String(Theme.warning), "an unverified write is not in the warning colour");
                verifyStableGeometry("verification could not run");
            } finally {
                widget.item.parseState(original);
                Proc.controlled = false;
                Proc.reset();
            }
        }

        function test_05_selection_and_feedback_do_not_depend_on_buttonBg_or_layout() {
            node("aurisTechnicalDisclosure").expanded = true;
            settle();
            const setup = settings();
            const confirmed = node("settingChoice_microphone_0");
            const requested = node("settingChoice_microphone_1");
            verify(confirmed !== null && requested !== null);
            verify(confirmed.backgroundColor === Theme.primary);
            verify(requested.backgroundColor !== Theme.primary, "unselected choices inherit a primary-valued buttonBg");
            verify(requested.textColor === Theme.surfaceText);

            setup.pendingRequests = {
                "microphone": {
                    "target": "left",
                    "state": "awaiting"
                },
                "press_speed": {
                    "target": "slower",
                    "state": "sending"
                }
            };
            compare(node("settingRow_microphone").requestText, "Waiting\u2026");
            compare(node("settingRow_press_speed").requestText, "Sending…");
            verify(confirmed.confirmedSelected && !confirmed.selected, "confirmed microphone was not kept separate from a request");
            verify(requested.requestedSelected && requested.selected, "requested microphone target was not selected immediately");

            const fixed = {
                "panelHeight": panel.item.implicitHeight,
                "settingsHeight": setup.implicitHeight,
                "surfaceHeight": node("advancedSettingsSurface").height,
                "setupContentHeight": node("aurisSetupScroll").contentHeight,
                "quickY": node("aurisQuickControls").mapToItem(scene, 0, 0).y,
                "batteryY": node("aurisMainScroll").mapToItem(scene, 0, 0).y,
                "setupY": node("aurisTechnicalDisclosure").mapToItem(scene, 0, 0).y,
                "feedbackHeight": node("aurisAdvancedFeedbackSlot").height
            };
            function verifyFixedGeometry(label) {
                compare(panel.item.implicitHeight, fixed.panelHeight, label + ": panel height changed");
                compare(setup.implicitHeight, fixed.settingsHeight, label + ": settings height changed");
                compare(node("advancedSettingsSurface").height, fixed.surfaceHeight, label + ": settings surface changed");
                compare(node("aurisSetupScroll").contentHeight, fixed.setupContentHeight, label + ": setup content changed");
                compare(node("aurisQuickControls").mapToItem(scene, 0, 0).y, fixed.quickY, label + ": quick controls moved");
                compare(node("aurisMainScroll").mapToItem(scene, 0, 0).y, fixed.batteryY, label + ": battery panel moved");
                compare(node("aurisTechnicalDisclosure").mapToItem(scene, 0, 0).y, fixed.setupY, label + ": setup disclosure moved");
                compare(node("aurisAdvancedFeedbackSlot").height, fixed.feedbackHeight, label + ": feedback slot changed");
            }

            for (const state of ["sending", "awaiting", "verifying"]) {
                setup.pendingRequests = {
                    "microphone": {
                        "target": "left",
                        "state": state
                    },
                    "press_speed": {
                        "target": "slower",
                        "state": state
                    },
                    "hold_duration": {
                        "target": "shorter",
                        "state": state
                    },
                    "listening_mode_cycle": {
                        "target": ["off", "anc"],
                        "state": state
                    },
                    "call_controls": {
                        "target": "hangup_once_mute_twice",
                        "state": state
                    },
                    "personalized_volume": {
                        "target": false,
                        "state": state
                    }
                };
                wait(20);
                verifyFixedGeometry("pending " + state);
            }

            const renameField = node("aurisRenameField");
            renameField.text = "";
            wait(20);
            compare(node("aurisRenameValidation").opacity, 1);
            verifyFixedGeometry("rename validation");
            renameField.text = setup.deviceName;

            const captionKeys = ["microphone", "press_speed", "hold_duration", "listening_mode_cycle", "call_controls", "personalized_volume"];
            for (const text of ["Verifying with AirPods\u2026", "Applied. AirPods 4 doesn't report this setting back.", ""]) {
                const captions = {};
                for (const key of captionKeys)
                    captions[key] = {
                        "text": text,
                        "color": Theme.error
                    };
                setup.captions = captions;
                wait(20);
                verifyFixedGeometry("caption \"" + text + "\"");
            }
            setup.captions = {};

            widget.item.showAdvancedToast("warning", "Sent, not verified.");
            const toast = node("aurisAdvancedToast");
            verify(toast !== null && toast.visible);
            compare(toast.opacity, 1);
            compare(toast.height, 52);
            compare(toast.color.a, 1, "toast permits underlying settings text to show through");
            verifyFixedGeometry("toast shown");
            verify(widget.item.advancedToastAccent("error") !== widget.item.advancedToastAccent("warning"), "error and warning toasts share a semantic colour");
            compare(node("aurisSetupDisclosureButton").tooltipText, null);
            widget.item.queuedToastText = "stale queued message";
            settle();
            const dismissPosition = node("aurisDismissToast").mapToItem(scene, 0, 0);
            verify(dismissPosition.y >= 0 && dismissPosition.y + node("aurisDismissToast").height <= panel.item.height, "toast close button outside rendered panel: " + dismissPosition.y + " / " + panel.item.height);
            mouseClick(node("aurisDismissToast"));
            compare(widget.item.advancedToastText, "");
            compare(widget.item.queuedToastText, "", "dismissed toast queue must not replay stale feedback");
            compare(toast.opacity, 0);
            verifyFixedGeometry("toast dismissed");
            setup.pendingRequests = {};
            setup.captions = {};
        }

        function test_06_seamless_switching() {
            const w = widget.item;
            const original = JSON.stringify(w.st);
            const snap = JSON.parse(original);
            const parse = () => {
                w.parseState(JSON.stringify(snap));
                settle();
            };
            Proc.controlled = true;
            Proc.reset();
            try {
                const slot = node("aurisHandoffSlot");
                const toggle = node("aurisHandoffToggle");
                const appleCaption = node("aurisHandoffAppleIdCaption");
                const rowText = node("aurisHandoffText");
                const useHere = node("aurisHandoffUseHere");
                verify(slot && toggle && appleCaption && rowText && useHere, "seamless switching controls missing");
                node("aurisTechnicalDisclosure").expanded = true;

                // Missing: an older daemon. Nothing new is usable or shown.
                parse();
                compare(w.handoff, null);
                compare(settings().handoff, null);
                verify(!toggle.enabled, "toggle usable without a handoff object");
                verify(!appleCaption.visible);
                compare(slot.height, 0);
                verify(!settings().submitHandoff(true));
                compare(Proc.calls.length, 0);

                // Disabled, without the Apple host ID.
                snap.handoff = {
                    "enabled": false,
                    "take_over_on_play": true,
                    "apple_host_id": false,
                    "owner": "unknown",
                    "audio_source": null,
                    "devices": [],
                    "last_event": null
                };
                parse();
                verify(toggle.enabled);
                verify(!toggle.checked);
                verify(appleCaption.visible, "missing Apple ID caption");
                compare(appleCaption.text, "Needs the Apple Bluetooth ID. See the README.");
                compare(appleCaption.color, Theme.warning);
                compare(slot.height, 0);
                toggle.toggled(true);
                compare(Proc.calls.length, 1);
                compare(Proc.calls[0].argv.slice(-2).join("|"), "handoff|on");
                snap.handoff.enabled = true;
                snap.handoff.apple_host_id = true;
                parse();
                verify(toggle.checked);
                verify(!appleCaption.visible);
                toggle.toggled(false);
                compare(Proc.calls[1].argv.slice(-2).join("|"), "handoff|off");
                node("aurisTechnicalDisclosure").expanded = false;
                settle();

                // Another device is playing: A2DP has gone, the AAP link stays.
                const fixedY = node("aurisQuickControls").y;
                snap.device.connected = false;
                snap.handoff.owner = "other";
                snap.handoff.audio_source = {
                    "address": "AA:BB:CC:DD:EE:01",
                    "is_local": false,
                    "state": "media"
                };
                snap.handoff.devices = [
                    {
                        "address": "AA:BB:CC:DD:EE:02",
                        "is_local": true
                    },
                    {
                        "address": "AA:BB:CC:DD:EE:01",
                        "is_local": false
                    }
                ];
                parse();
                verify(w.connected, "a handed-off AAP link read as a disconnect");
                verify(w.wantVisible, "pill hid while the AirPods were on another device");
                compare(slot.height, node("aurisHandoffCard").height + Theme.spacingS);
                compare(rowText.text, "Playing on another device");
                verify(useHere.visible);
                verify(useHere.x >= 0 && useHere.x + useHere.width <= useHere.parent.width + 0.1, "Use here outside its row: x " + useHere.x + " w " + useHere.width + " row " + useHere.parent.width + " card " + node("aurisHandoffCard").width);
                checkButton(useHere);
                compare(node("aurisQuickControls").y, fixedY);
                const before = Proc.calls.length;
                mouseClick(useHere);
                tryVerify(() => Proc.calls.length === before + 1, 1000, "Use here never reached the command runner");
                compare(Proc.calls[before].argv[Proc.calls[before].argv.length - 1], "take-over");

                snap.handoff.audio_source.state = "call";
                parse();
                compare(rowText.text, "On a call on another device");
                snap.handoff.audio_source.state = "idle";
                parse();
                compare(slot.height, 0);

                // Local owner with another device known: nothing extra.
                snap.device.connected = true;
                snap.handoff.owner = "local";
                snap.handoff.audio_source = {
                    "address": "AA:BB:CC:DD:EE:02",
                    "is_local": true,
                    "state": "media"
                };
                parse();
                verify(!w.handoffCardVisible);
                compare(slot.height, 0);

                const captions = [["yielded", "Moved to your other device"], ["took_over", "Moved here"], ["yield_requested", "Handing off\u2026"]];
                for (let i = 0; i < captions.length; i++) {
                    snap.handoff.last_event = {
                        "kind": captions[i][0],
                        "at": new Date(Date.now() + i).toISOString(),
                        "peer": "AA:BB:CC:DD:EE:01"
                    };
                    parse();
                    compare(w.handoffCaption, captions[i][1]);
                    compare(rowText.text, captions[i][1]);
                    verify(slot.height > 0);
                    verify(!useHere.visible);
                }
                const cardHeight = node("aurisHandoffCard").height;
                tryVerify(() => w.handoffCaption === "", 5000, "handoff caption never retired");
                tryCompare(slot, "height", 0);
                compare(node("aurisHandoffCard").height, cardHeight);
                // The same event arriving again is not news.
                parse();
                compare(w.handoffCaption, "");
            } finally {
                node("aurisTechnicalDisclosure").expanded = false;
                w.parseState(original);
                Proc.controlled = false;
                Proc.reset();
            }
        }

        // The daemon can spend several seconds rejoining the AirPods after a
        // bud role switch, an eviction or a Low Energy wake. The module has to
        // stay on screen and say what it is waiting for, because disappearing
        // is indistinguishable from the AirPods having been put away.
        function test_07_link_healing_keeps_the_module_on_screen() {
            const w = widget.item;
            const original = JSON.stringify(w.st);
            const snap = JSON.parse(original);
            const since = new Date().toISOString();
            const parse = () => {
                w.parseState(JSON.stringify(snap));
                settle();
            };
            const reconnect = (reason, attempt) => {
                snap.link = {
                    "status": "reconnecting",
                    "reason": reason,
                    "attempt": attempt,
                    "since": since
                };
            };
            Proc.controlled = true;
            Proc.reset();
            try {
                const slot = node("aurisLinkSlot");
                const row = node("aurisLinkText");
                verify(slot && row, "link status row missing");

                // Absent: an older daemon. Everything reads as it did before
                // the field existed.
                delete snap.link;
                parse();
                compare(w.link, null);
                compare(w.linkStatus, "");
                verify(!w.linkReconnecting, "a missing link object was read as reconnecting");
                compare(w.linkStatusText, "");
                compare(slot.height, 0);
                verify(w.connected && w.moduleShouldShow && w.wantVisible);
                verify(!w.disconnectGrace);
                compare(w.barContentOpacity, 1);
                verify(!w.cellDim("left") && !w.cellDim("right") && !w.cellDim("case"));

                // A pending write waits for the answer instead of being
                // cancelled, exactly as it does through a verification reopen.
                node("aurisTechnicalDisclosure").expanded = true;
                settle();
                mouseClick(node("settingChoice_press_speed_1"));
                tryVerify(() => Proc.calls.length === 1);
                Proc.complete(0, '{"ok":true}', 0);
                compare(w.pendingAdvancedFor("press_speed").state, "awaiting");
                snap.device.connected = false;
                snap.device.aap_link = false;
                reconnect("link_lost", 1);
                parse();
                verify(w.pendingAdvancedFor("press_speed") !== null, "a scheduled reconnect cancelled a pending write");
                compare(w.pendingAdvancedFor("press_speed").state, "awaiting");
                node("aurisTechnicalDisclosure").expanded = false;
                settle();

                // Reconnecting: visible, dimmed, and one neutral line. The
                // daemon's reason is a guess at a cause, so it never reaches
                // the screen; every reason reads the same.
                const reasons = ["bud_switch", "taken_over", "link_lost", "auto_connect"];
                const caption = "Attempting to reconnect";
                for (const reason of reasons) {
                    reconnect(reason, 1);
                    parse();
                    verify(w.linkReconnecting, reason + ": not read as reconnecting");
                    verify(!w.deviceConnected, reason + ": the raw device state was lost");
                    verify(w.connected, reason + ": a reconnect was rendered as a disconnect");
                    verify(w.moduleShouldShow && w.wantVisible, reason + ": the module hid itself while the link was healing");
                    verify(!w.disconnectGrace, reason + ": an announced reconnect fell back to the grace");
                    compare(w.linkReason, reason, reason + ": the daemon's reason was lost");
                    compare(w.linkStatusText, caption);
                    compare(row.text, caption);
                    verify(slot.height > 0, reason + ": the status row stayed closed");
                    verify(w.barContentOpacity < 1, reason + ": the bar icon was left untouched");
                    verify(w.stale, reason + ": held readings were not marked stale");
                    verify(w.cellDim("left") && w.cellDim("right") && w.cellDim("case"), reason + ": held readings were not dimmed");
                    compare(node("batteryLeft").level, 85, reason + ": the left row lost its last reading");
                    compare(node("batteryRight").level, 90, reason + ": the right row lost its last reading");
                    compare(node("batteryCase").level, 75, reason + ": the case row lost its last reading");
                    verify(node("batteryLeft").dim && node("batteryRight").dim && node("batteryCase").dim, reason + ": a battery row rendered as live");
                }

                // The attempt is only worth reading once a first try failed,
                // and it is the only thing appended to the caption.
                reconnect("link_lost", 2);
                parse();
                compare(w.linkStatusText, caption + " \u00b7 attempt 2");
                compare(row.text, caption + " \u00b7 attempt 2");
                reconnect(null, 2);
                parse();
                compare(w.linkStatusText, caption + " \u00b7 attempt 2");
                reconnect(null, 1);
                parse();
                compare(w.linkStatusText, caption);
                reconnect(null, 0);
                parse();
                compare(w.linkStatusText, caption);

                // Given up: held on screen for the grace, then gone.
                snap.link = {
                    "status": "disconnected",
                    "reason": "link_lost",
                    "attempt": 3,
                    "since": since
                };
                parse();
                verify(!w.connected, "a finished disconnect still read as connected");
                verify(!w.linkReconnecting);
                compare(w.linkStatusText, "");
                verify(w.disconnectGrace, "the disconnect grace never started");
                verify(w.moduleShouldShow, "the module vanished the instant the link dropped");
                tryCompare(slot, "height", 0);
                tryVerify(() => !w.disconnectGrace, 4000, "the disconnect grace never expired");
                verify(!w.moduleShouldShow, "the module stayed on screen after the grace");

                // An unexplained disconnect from an older daemon gets the same
                // grace, with no link object anywhere.
                snap.device.connected = true;
                snap.device.aap_link = true;
                delete snap.link;
                parse();
                verify(w.connected && w.moduleShouldShow && !w.disconnectGrace);
                snap.device.connected = false;
                snap.device.aap_link = false;
                parse();
                verify(!w.connected, "an old-daemon disconnect was read as connected");
                verify(w.disconnectGrace, "an unexplained disconnect got no grace");
                verify(w.moduleShouldShow, "an unexplained disconnect hid the module immediately");
                tryVerify(() => !w.moduleShouldShow, 4000, "the module stayed on screen after the grace");

                // Back: the line goes away and the readings are live again.
                snap.device.connected = true;
                snap.device.aap_link = true;
                snap.link = {
                    "status": "connected",
                    "reason": null,
                    "attempt": 0,
                    "since": since
                };
                parse();
                verify(w.connected && w.moduleShouldShow && w.wantVisible);
                verify(!w.disconnectGrace, "the grace outlived the reconnection");
                compare(w.linkStatusText, "");
                compare(w.barContentOpacity, 1);
                verify(!w.stale, "a healed link left the readings dimmed");
                verify(!w.cellDim("left") && !w.cellDim("right") && !w.cellDim("case"));
                tryCompare(slot, "height", 0);
            } finally {
                node("aurisTechnicalDisclosure").expanded = false;
                w.parseState(original);
                Proc.controlled = false;
                Proc.reset();
            }
        }

        // An open panel used to outlive the AirPods: the bar module fell back
        // to the plain Bluetooth icon and then hid itself, while the popout
        // went on offering controls for a device that was back in its case.
        function test_08_panel_closes_once_the_airpods_are_gone() {
            const w = widget.item;
            const original = JSON.stringify(w.st);
            const snap = JSON.parse(original);
            const since = new Date().toISOString();
            const parse = () => {
                w.parseState(JSON.stringify(snap));
                settle();
            };
            try {
                host.shouldBeVisible = true;
                settle();
                compare(w.popoutRef, host, "the panel never handed its host popout up");
                verify(w.panelOpen, "an open panel was not read as open");
                verify(!w.panelShouldClose, "a connected device asked for the panel to close");
                const closes = w.closePopoutCount;

                // Healing the link holds the panel open right through the
                // grace: that is the moment the controls are most wanted.
                snap.device.connected = false;
                snap.device.aap_link = false;
                snap.link = {
                    "status": "reconnecting",
                    "reason": "link_lost",
                    "attempt": 1,
                    "since": since
                };
                parse();
                verify(w.panelHeldOpen, "a reconnect was not read as a held link");
                verify(!w.panelShouldClose, "a reconnect asked for the panel to close");
                wait(w.disconnectGraceMs + 400);
                compare(w.closePopoutCount, closes, "the panel closed while the daemon was still reconnecting");
                verify(w.panelOpen, "the panel was closed during a reconnect");

                // A settings read-back reopen is not a disconnect.
                delete snap.link;
                snap.verify_reopen = true;
                parse();
                verify(w.verifyReopen && !w.panelShouldClose, "a verification reopen asked for the panel to close");
                delete snap.verify_reopen;

                // Neither is audio simply playing on another host.
                snap.device.aap_link = true;
                snap.handoff = Object.assign({}, snap.handoff, {
                    "owner": "other",
                    "audio_source": {
                        "address": "AA:BB:CC:DD:EE:01",
                        "is_local": false,
                        "state": "media"
                    }
                });
                parse();
                verify(w.handoffHeldElsewhere && !w.panelShouldClose, "a handoff asked for the panel to close");
                compare(w.closePopoutCount, closes, "the panel closed without a disconnect");

                // Gone: held for the grace, then closed with the module.
                snap.handoff.owner = "local";
                snap.device.aap_link = false;
                snap.link = {
                    "status": "disconnected",
                    "reason": "link_lost",
                    "attempt": 3,
                    "since": since
                };
                parse();
                verify(!w.connected && w.disconnectGrace, "a dropped link skipped the grace");
                verify(!w.panelShouldClose, "the panel was given up on inside the grace");
                compare(w.closePopoutCount, closes, "the panel closed before the grace expired");
                tryVerify(() => !w.moduleShouldShow, 4000, "the module stayed on screen after the grace");
                verify(w.panelShouldClose, "the module hid itself with the panel still held open");
                compare(w.closePopoutCount, closes + 1, "the panel was left open after the AirPods went away");
            } finally {
                host.shouldBeVisible = true;
                w.parseState(original);
                settle();
            }
        }
    }
}
