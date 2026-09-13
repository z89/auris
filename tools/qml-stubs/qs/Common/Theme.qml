pragma Singleton
import QtQuick

QtObject {
    readonly property int spacingXS: 4
    readonly property int spacingS: 6
    readonly property int spacingM: 10
    readonly property int spacingL: 14
    readonly property int fontSizeSmall: 12
    property int fontSizeMedium: 14
    readonly property int fontSizeXLarge: 22
    readonly property int iconSizeSmall: 18
    readonly property int iconSize: 22
    readonly property int cornerRadius: 10
    readonly property int layerOutlineWidth: 1
    property color surfaceText: "#f4f4f5"
    property color surfaceVariantText: "#b6b8bf"
    property color floatingWindowNestedSurface: "#2b3038"
    property color surfaceContainerHigh: "#272c34"
    property color surfaceVariant: "#49515d"
    property color outlineMedium: "#59616d"
    property color primary: "#9fc9ff"
    property color primaryText: "#10233a"
    property color success: "#66d18f"
    property color warning: "#f4ba67"
    property color error: "#ee7777"
    property color buttonBg: "#3a424d"
    property color buttonText: "#f4f4f5"
    property color widgetTextColor: "#f4f4f5"
    readonly property string fontFamily: "sans-serif"
    readonly property int shortDuration: 1
    readonly property int mediumDuration: 1
    readonly property int standardEasing: 0
    readonly property int emphasizedEasing: 0

    function withAlpha(colorValue, alpha) {
        return Qt.rgba(colorValue.r, colorValue.g, colorValue.b, alpha);
    }
}
