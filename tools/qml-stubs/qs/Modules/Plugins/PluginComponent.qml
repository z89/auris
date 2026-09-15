import QtQuick

Item {
    id: root

    property var pluginData: ({
            "showPercent": true,
            "pillValue": "buds",
            "lowThreshold": 20,
            "criticalThreshold": 10,
            "hideWhenDisconnected": false,
            "ctlCommand": "auris-headless-disabled"
        })
    property var pluginService: null
    property string pluginId: "auris-headless"
    property var parentScreen: null
    property real barThickness: 48
    property int iconSize: 20
    property Component horizontalBarPill: null
    property Component verticalBarPill: null
    property Component popoutContent: null
    property real popoutWidth: 400
    property real popoutHeight: 0
    property var pillRightClickAction: null
    property string ccWidgetIcon: ""
    property string ccWidgetPrimaryText: ""
    property string ccWidgetSecondaryText: ""
    property bool ccWidgetIsActive: false
    property Component ccDetailContent: null
    property real ccDetailHeight: 250
    signal ccWidgetToggled
    property bool visibilityOverride: true
    function setVisibilityOverride(value) {
        visibilityOverride = value;
    }
    // The real component forwards this to its popout's close(). The stub
    // records it instead, so a test can tell an automatic close from a
    // panel that simply stopped being asked for.
    property int closePopoutCount: 0
    signal popoutCloseRequested

    function closePopout() {
        closePopoutCount += 1;
        popoutCloseRequested();
    }
}
