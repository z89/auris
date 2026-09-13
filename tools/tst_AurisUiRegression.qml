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
                settings_api: 1,
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
                }
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
            snapshot.settings_report_seq = {
                "press_speed": 1
            };
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
                    snapshot.settings.press_speed = choice[1];
                    snapshot.settings_report_seq.press_speed++;
                    widget.item.parseState(JSON.stringify(snapshot));
                    compare(widget.item.pendingAdvancedFor("press_speed"), null);
                    compare(widget.item.confirmedSetting("press_speed"), choice[1]);
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

        function test_03c_missing_echo_reopens_only_control_link_once() {
            const original = JSON.stringify(widget.item.st);
            const snapshot = JSON.parse(original);
            snapshot.settings.press_speed = "default";
            snapshot.settings_report_seq = {
                "press_speed": 1
            };
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
                    "setupY": node("aurisTechnicalDisclosure").mapToItem(scene, 0, 0).y
                };
                function verifyBatteryGeometry(label) {
                    compare(panel.item.implicitHeight, stableGeometry.panelHeight, label + ": panel height changed");
                    compare(node("aurisMainScroll").contentHeight, stableGeometry.batteryContentHeight, label + ": battery content height changed");
                    compare(node("batteryLeft").height, stableGeometry.batteryLeftHeight, label + ": left battery row changed");
                    compare(node("batteryRight").height, stableGeometry.batteryRightHeight, label + ": right battery row changed");
                    compare(node("batteryCase").height, stableGeometry.batteryCaseHeight, label + ": case battery row changed");
                    compare(node("aurisTechnicalDisclosure").mapToItem(scene, 0, 0).y, stableGeometry.setupY, label + ": setup moved");
                }
                mouseClick(node("settingChoice_press_speed_1"));
                tryVerify(() => Proc.calls.length === 1);
                Proc.complete(0, '{"ok":true}', 0);
                const pending = widget.item.pendingAdvancedFor("press_speed");
                pending.sentAt = Date.now() - 1500;
                widget.item.setAdvancedPending("press_speed", pending);
                verify(widget.item.maybeStartAdvancedReadback(Date.now()));
                compare(Proc.calls.length, 2);
                compare(Proc.calls[1].argv.slice(-1)[0], "reconnect");
                compare(widget.item.pendingAdvancedFor("press_speed").state, "verifying");
                verify(!widget.item.maybeStartAdvancedReadback(Date.now()), "readback reopened twice");

                const down = JSON.parse(JSON.stringify(snapshot));
                down.device.aap_link = false;
                down.settings = {};
                down.battery.stale = true;
                for (const side of ["left", "right", "case"]) {
                    down.battery[side].source = "aap";
                    down.battery[side].fresh = false;
                    down.battery[side].present = false;
                    down.battery[side].charging = false;
                    down.battery[side].last_known_charging = false;
                    down.battery[side].last_seen = new Date().toISOString();
                }
                widget.item.parseState(JSON.stringify(down));
                wait(20);
                verify(widget.item.advancedReadbackSawDown);
                verify(widget.item.pendingAdvancedFor("press_speed") !== null, "expected readback link close cancelled the request");
                compare(node("batteryLeft").caption.indexOf("last seen"), 0);
                verifyBatteryGeometry("AAP readback link down");
                Proc.complete(1, '{"ok":true}', 0);

                snapshot.settings.press_speed = "slower";
                snapshot.settings_report_seq.press_speed = 2;
                widget.item.parseState(JSON.stringify(snapshot));
                compare(widget.item.pendingAdvancedFor("press_speed"), null);
                verify(!widget.item.advancedReadbackActive);
                compare(widget.item.confirmedSetting("press_speed"), "slower");
                verifyBatteryGeometry("AAP readback restored");
            } finally {
                widget.item.advancedReadbackActive = false;
                widget.item.advancedReadbackSawDown = false;
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
            compare(node("settingRow_microphone").requestText, "Pending");
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

            for (const state of ["sending", "awaiting", "verifying", "unconfirmed"]) {
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

            widget.item.showAdvancedToast("warning", "Sent; no device report yet.");
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
        }
    }
}
