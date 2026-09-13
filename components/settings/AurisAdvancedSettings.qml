pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Controls
import qs.Common
import qs.Widgets

Item {
    id: root

    // Kept in a dedicated directory so a running shell can discover this
    // component even if it indexed the plugin root before installation.
    // Content-sized so the main panel owns the single scrollable viewport.
    implicitHeight: settingsColumn.y + settingsColumn.implicitHeight
    height: implicitHeight

    property bool available: false
    property string unavailableReason: ""
    // Confirmed values above are intentionally never replaced by a request.
    // This map only annotates the control that is waiting for a device report.
    property var pendingRequests: ({})
    property string deviceIdentity: ""
    property string deviceName: ""
    property var microphone: null
    property var pressSpeed: null
    property var holdDuration: null
    property var listeningModeCycle: null
    property var callControls: null
    property var personalizedVolume: null

    signal renameRequested(string name)
    signal settingRequested(string key, var value)

    readonly property var cycleModes: ["off", "anc", "transparency", "adaptive"]
    property var cycleDraft: []
    property bool cycleEdited: false
    property string cycleNotice: ""
    property string activeHelpKey: ""

    FontMetrics {
        id: labelMetrics
        font.family: Theme.fontFamily
        font.pixelSize: Theme.fontSizeMedium
        font.weight: Font.Medium
    }

    function fittingColumns(labels, preferred, availableWidth) {
        const widest = Math.max.apply(null, labels.map(label => labelMetrics.advanceWidth(label)));
        // advanceWidth() is a method, not a notifying property. Read font
        // explicitly so a font-only change reflows even a screen-clamped grid.
        const minimum = Math.max(64, labelMetrics.font.pixelSize * 2, widest + Theme.spacingM * 2);
        return Math.max(1, Math.min(preferred, Math.floor((availableWidth + Theme.spacingS) / (minimum + Theme.spacingS))));
    }

    function utf8ByteLength(value) {
        let bytes = 0;
        for (let i = 0; i < value.length; i++) {
            const code = value.charCodeAt(i);
            if (code <= 0x7f) {
                bytes++;
            } else if (code <= 0x7ff) {
                bytes += 2;
            } else if (code >= 0xd800 && code <= 0xdbff) {
                if (i + 1 >= value.length)
                    return -1;
                const low = value.charCodeAt(++i);
                if (low < 0xdc00 || low > 0xdfff)
                    return -1;
                bytes += 4;
            } else if (code >= 0xdc00 && code <= 0xdfff) {
                return -1;
            } else {
                bytes += 3;
            }
        }
        return bytes;
    }

    function renameValidationError(value) {
        if (value.trim().length === 0)
            return "Enter a non-blank name.";
        for (let i = 0; i < value.length; i++) {
            const code = value.charCodeAt(i);
            if (code <= 0x1f || (code >= 0x7f && code <= 0x9f))
                return "Control characters are not allowed.";
        }
        const bytes = utf8ByteLength(value);
        if (bytes < 0)
            return "The name contains invalid Unicode.";
        if (bytes > 255)
            return "The name must be at most 255 UTF-8 bytes.";
        return "";
    }

    function normalizedCycle(value) {
        if (!Array.isArray(value))
            return [];
        return cycleModes.filter(mode => value.indexOf(mode) >= 0);
    }

    function pendingFor(key) {
        return pendingRequests && pendingRequests[key] ? pendingRequests[key] : null;
    }

    function pendingText(key) {
        const pending = pendingFor(key);
        if (!pending)
            return "";
        if (pending.state === "sending")
            return "Sending…";
        if (pending.state === "verifying")
            return "Verifying…";
        if (pending.state === "unconfirmed")
            return "Unconfirmed";
        return "Pending";
    }

    function toggleCycleMode(mode) {
        const next = normalizedCycle(cycleDraft);
        const index = next.indexOf(mode);
        if (index >= 0) {
            // Once a reported or requested cycle is valid, never leave the UI
            // showing an unsent one-mode divergence. An unknown cycle may still
            // establish its first choice and ask for a second.
            if (next.length <= 2 && normalizedCycle(cycleDraft).length >= 2) {
                cycleNotice = "Keep at least two modes in the cycle.";
                return false;
            }
            next.splice(index, 1);
        } else {
            next.push(mode);
        }
        cycleDraft = normalizedCycle(next);
        cycleEdited = true;
        cycleNotice = "";
        return true;
    }

    function resetCycleDraft() {
        cycleDraft = normalizedCycle(listeningModeCycle);
        cycleEdited = false;
        cycleNotice = "";
    }

    function submitRename(value) {
        const error = renameValidationError(value);
        if (!available || error.length > 0)
            return false;
        renameRequested(value);
        return true;
    }

    function submitSetting(key, value) {
        if (!available)
            return false;
        settingRequested(key, value);
        return true;
    }

    function cycleLabel(value) {
        switch (value) {
        case "off":
            return "Off";
        case "anc":
            return "ANC";
        case "transparency":
            return "Transparency";
        case "adaptive":
            return "Adaptive";
        default:
            return String(value);
        }
    }

    function cycleText(value) {
        const normalized = normalizedCycle(value);
        if (normalized.length === 0)
            return "Not reported";
        return normalized.map(mode => cycleLabel(mode)).join(", ");
    }

    onDeviceIdentityChanged: {
        activeHelpKey = "";
        resetCycleDraft();
        Qt.callLater(() => renameField.text = root.deviceName);
    }
    onDeviceNameChanged: {
        renameField.text = deviceName;
    }
    onAvailableChanged: {
        if (!available)
            resetCycleDraft();
    }
    onVisibleChanged: {
        if (!visible)
            activeHelpKey = "";
    }
    onListeningModeCycleChanged: {
        const reported = normalizedCycle(listeningModeCycle);
        if (!cycleEdited || JSON.stringify(reported) === JSON.stringify(normalizedCycle(cycleDraft))) {
            cycleDraft = reported;
            cycleEdited = false;
        }
    }
    Component.onCompleted: resetCycleDraft()

    component InfoButton: DankActionButton {
        id: infoButton

        required property string helpKey
        required property string helpText
        readonly property bool helpOpen: root.activeHelpKey === helpKey
        readonly property bool tooltipVisible: clickTooltip.visible
        readonly property var helpPopup: clickTooltip

        objectName: "settingsHelp_" + helpKey
        buttonSize: 28
        iconSize: 16
        iconName: "info"
        iconColor: Theme.surfaceVariantText
        backgroundColor: "transparent"
        // Never start DMS's separate hover tooltip; clearing its text after
        // hover does not close it. This button has one click-controlled popup.
        tooltipText: null
        onClicked: root.activeHelpKey = root.activeHelpKey === helpKey ? "" : helpKey

        ToolTip {
            id: clickTooltip
            objectName: "settingsTooltip_" + infoButton.helpKey

            parent: infoButton
            x: infoButton.width - width
            y: infoButton.height + Theme.spacingXS
            width: Math.min(300, root.width - Theme.spacingM * 2)
            margins: Theme.spacingS
            focus: true
            visible: root.visible && infoButton.helpOpen
            timeout: -1
            onClosed: {
                if (root.activeHelpKey === infoButton.helpKey)
                    root.activeHelpKey = "";
            }

            contentItem: Text {
                text: infoButton.helpText
                wrapMode: Text.WordWrap
                color: Theme.surfaceText
                font.pixelSize: Theme.fontSizeSmall
            }

            background: Rectangle {
                radius: Theme.cornerRadius
                color: Theme.surfaceContainerHigh
                border.color: Theme.outlineMedium
                border.width: Theme.layerOutlineWidth
            }
        }
    }

    component SettingHeader: Item {
        id: settingHeader

        required property string settingKey
        required property string title
        required property string helpText
        property bool unknown: false
        property string requestText: ""

        height: 28

        StyledText {
            anchors.left: parent.left
            anchors.right: stateLabel.left
            anchors.rightMargin: Theme.spacingS
            anchors.verticalCenter: parent.verticalCenter
            text: settingHeader.title
            elide: Text.ElideRight
            font.pixelSize: Theme.fontSizeMedium
            font.weight: Font.Medium
            color: Theme.surfaceText
        }

        StyledText {
            id: stateLabel

            // Every row permanently reserves the same status lane. Pending,
            // verifying and unknown labels may change its contents, but never
            // the row geometry or the space available to the setting title.
            width: 96
            anchors.right: infoButton.left
            anchors.rightMargin: Theme.spacingXS
            anchors.verticalCenter: parent.verticalCenter
            text: settingHeader.requestText.length > 0 ? settingHeader.requestText : settingHeader.unknown ? "Not reported" : ""
            opacity: text.length > 0 ? 1 : 0
            horizontalAlignment: Text.AlignRight
            elide: Text.ElideRight
            font.pixelSize: Theme.fontSizeSmall - 1
            color: Theme.warning
        }

        InfoButton {
            id: infoButton

            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            helpKey: settingHeader.settingKey
            helpText: settingHeader.helpText
        }
    }

    component ChoiceRow: Item {
        id: choiceRow

        required property string settingKey
        required property string title
        required property string helpText
        property var labels: []
        property var values: []
        property var confirmedValue: null
        property int columns: labels.length
        readonly property int effectiveColumns: root.fittingColumns(labels, columns, width)
        readonly property bool confirmedKnown: values.indexOf(confirmedValue) >= 0
        readonly property var pending: root.pendingFor(settingKey)
        readonly property int requestedIndex: pending ? values.indexOf(pending.target) : -1
        // The selected button already identifies the requested value. Keep the
        // fixed header lane short so a lifecycle label can never wrap or reflow.
        readonly property string requestText: pending ? root.pendingText(settingKey) : ""
        signal choiceRequested(var value)

        objectName: "settingRow_" + settingKey
        width: parent ? parent.width : 0
        height: choiceContent.implicitHeight

        Column {
            id: choiceContent

            width: parent.width
            spacing: Theme.spacingXS

            SettingHeader {
                width: parent.width
                settingKey: choiceRow.settingKey
                title: choiceRow.title
                helpText: choiceRow.helpText
                unknown: !choiceRow.confirmedKnown
                requestText: choiceRow.requestText
            }

            Grid {
                id: choices

                width: parent.width
                height: implicitHeight
                spacing: Theme.spacingS
                columns: choiceRow.effectiveColumns

                Repeater {
                    model: choiceRow.labels.length

                    DankButton {
                        required property int index

                        readonly property bool confirmedSelected: choiceRow.confirmedValue !== null && choiceRow.confirmedValue !== undefined && choiceRow.values[index] === choiceRow.confirmedValue
                        readonly property bool requestedSelected: choiceRow.pending !== null && choiceRow.requestedIndex === index
                        // A request is the immediate visual choice; confirmedValue
                        // remains separate and is never overwritten by it.
                        readonly property bool selected: requestedSelected || (!choiceRow.pending && confirmedSelected)
                        objectName: "settingChoice_" + choiceRow.settingKey + "_" + index
                        width: (choices.width - choices.spacing * (choices.columns - 1)) / choices.columns
                        text: choiceRow.labels[index]
                        buttonHeight: 32
                        horizontalPadding: Theme.spacingM
                        enabled: root.available
                        // buttonBg may be the primary colour in DMS themes.
                        // Keep an unselected choice on a neutral surface.
                        backgroundColor: selected ? Theme.primary : Theme.surfaceContainerHigh
                        textColor: selected ? Theme.primaryText : Theme.surfaceText
                        onClicked: choiceRow.choiceRequested(choiceRow.values[index])
                    }
                }
            }
        }
    }

    component SettingSeparator: Rectangle {
        width: parent ? parent.width : 0
        height: Theme.layerOutlineWidth
        color: Theme.withAlpha(Theme.outlineMedium, 0.6)
    }

    Column {
        id: settingsColumn

        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        spacing: Theme.spacingS

        StyledRect {
            objectName: "advancedSettingsSurface"
            width: parent.width
            height: compactSettings.implicitHeight + Theme.spacingL * 2
            radius: Theme.cornerRadius
            color: Theme.floatingWindowNestedSurface
            border.color: Theme.outlineMedium
            border.width: Theme.layerOutlineWidth

            Column {
                id: compactSettings

                anchors.fill: parent
                anchors.margins: Theme.spacingL
                spacing: Theme.spacingXS

                Column {
                    width: parent.width
                    spacing: Theme.spacingXS

                    SettingHeader {
                        width: parent.width
                        settingKey: "rename"
                        title: "Device name"
                        helpText: "Requests an accessory name change. The new name is shown only after the AirPods report it back."
                        unknown: root.deviceName.length === 0
                        requestText: root.pendingText("rename")
                    }

                    Row {
                        width: parent.width
                        spacing: Theme.spacingS

                        DankTextField {
                            id: renameField

                            objectName: "aurisRenameField"
                            width: parent.width - renameButton.width - parent.spacing
                            height: 36
                            text: root.deviceName
                            placeholderText: "AirPods name"
                            maximumLength: 255
                            showClearButton: true
                            enabled: root.available
                            onAccepted: root.submitRename(text)
                        }

                        DankButton {
                            id: renameButton

                            text: "Rename"
                            buttonHeight: 36
                            enabled: root.available && root.renameValidationError(renameField.text).length === 0 && (renameField.text !== root.deviceName || root.pendingFor("rename") !== null)
                            onClicked: root.submitRename(renameField.text)
                        }
                    }

                    StyledText {
                        objectName: "aurisRenameValidation"
                        width: parent.width
                        height: 16
                        text: root.renameValidationError(renameField.text)
                        opacity: renameField.text !== root.deviceName && text.length > 0 ? 1 : 0
                        elide: Text.ElideRight
                        font.pixelSize: Theme.fontSizeSmall - 1
                        color: Theme.warning
                    }
                }

                SettingSeparator {}

                ChoiceRow {
                    settingKey: "microphone"
                    title: "Microphone"
                    helpText: "Chooses the preferred left or right AirPod microphone. It does not change the Bluetooth microphone codec or call quality."
                    labels: ["Auto", "Left", "Right"]
                    values: ["auto", "left", "right"]
                    confirmedValue: root.microphone
                    columns: 3
                    onChoiceRequested: value => root.submitSetting("microphone", value)
                }

                SettingSeparator {}

                ChoiceRow {
                    settingKey: "press_speed"
                    title: "Press speed"
                    helpText: "Changes how quickly multiple stem presses must follow one another. It does not change what a press does."
                    labels: ["Default", "Slower", "Slowest"]
                    values: ["default", "slower", "slowest"]
                    confirmedValue: root.pressSpeed
                    columns: 3
                    onChoiceRequested: value => root.submitSetting("press_speed", value)
                }

                SettingSeparator {}

                ChoiceRow {
                    settingKey: "hold_duration"
                    title: "Hold duration"
                    helpText: "Changes how long you hold the stem before its configured hold action starts. It does not change the action."
                    labels: ["Default", "Shorter", "Shortest"]
                    values: ["default", "shorter", "shortest"]
                    confirmedValue: root.holdDuration
                    columns: 3
                    onChoiceRequested: value => root.submitSetting("hold_duration", value)
                }

                SettingSeparator {}

                Column {
                    width: parent.width
                    spacing: Theme.spacingXS

                    SettingHeader {
                        width: parent.width
                        settingKey: "listening_cycle"
                        title: "Listening-mode cycle"
                        helpText: "Chooses which modes are included when cycling with a stem hold. Auris sends a set of at least two modes; it does not promise a custom order."
                        unknown: root.normalizedCycle(root.listeningModeCycle).length === 0
                        requestText: root.pendingText("listening_mode_cycle")
                    }

                    Grid {
                        id: cycleChoices

                        width: parent.width
                        height: implicitHeight
                        spacing: Theme.spacingS
                        columns: root.fittingColumns(root.cycleModes.map(root.cycleLabel), 4, width)

                        Repeater {
                            model: root.cycleModes.length

                            DankButton {
                                required property int index

                                readonly property string mode: root.cycleModes[index]
                                readonly property bool selected: root.normalizedCycle(root.cycleDraft).indexOf(mode) >= 0
                                objectName: "settingChoice_listening_cycle_" + index
                                width: (cycleChoices.width - cycleChoices.spacing * (cycleChoices.columns - 1)) / cycleChoices.columns
                                text: root.cycleLabel(mode)
                                buttonHeight: 32
                                horizontalPadding: Theme.spacingM
                                enabled: root.available
                                backgroundColor: selected ? Theme.primary : Theme.surfaceContainerHigh
                                textColor: selected ? Theme.primaryText : Theme.surfaceText
                                onClicked: {
                                    if (!root.toggleCycleMode(mode))
                                        return;
                                    const cycle = root.normalizedCycle(root.cycleDraft);
                                    if (cycle.length >= 2)
                                        root.submitSetting("listening_mode_cycle", cycle);
                                }
                            }
                        }
                    }

                    StyledText {
                        width: parent.width
                        height: 16
                        text: root.cycleNotice.length > 0 ? root.cycleNotice : root.cycleDraft.length < 2 ? "Select at least two modes to send" : "Requested changes send immediately; device report remains authoritative."
                        elide: Text.ElideRight
                        font.pixelSize: Theme.fontSizeSmall - 1
                        color: root.cycleNotice.length > 0 || root.cycleDraft.length < 2 ? Theme.warning : Theme.surfaceVariantText
                    }
                }

                SettingSeparator {}

                ChoiceRow {
                    settingKey: "call_controls"
                    title: "Call controls"
                    helpText: "Configures mute and end-call press gestures together. The two gestures cannot be changed independently."
                    labels: ["1× mute · 2× end call", "1× end call · 2× mute"]
                    values: ["mute_once_hangup_twice", "hangup_once_mute_twice"]
                    confirmedValue: root.callControls
                    columns: 2
                    onChoiceRequested: value => root.submitSetting("call_controls", value)
                }

                SettingSeparator {}

                ChoiceRow {
                    settingKey: "personalized_volume"
                    title: "Personalized Volume"
                    helpText: "Requests the AirPods Personalized Volume setting. Compatible firmware may adapt volume, but a noticeable effect on Linux is not guaranteed."
                    labels: ["Off", "On"]
                    values: [false, true]
                    confirmedValue: root.personalizedVolume
                    columns: 2
                    onChoiceRequested: value => root.submitSetting("personalized_volume", value)
                }
            }
        }
    }
}
