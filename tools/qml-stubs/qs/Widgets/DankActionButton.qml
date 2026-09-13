import QtQuick

Rectangle {
    property string iconName: ""
    property int iconSize: 18
    property color iconColor: "white"
    property color backgroundColor: "transparent"
    property int buttonSize: 36
    property var tooltipText: null
    signal clicked
    width: buttonSize
    height: buttonSize
    color: backgroundColor
}
