import QtQuick
import qs.Common

Rectangle {
    property string text: ""
    property string iconName: ""
    property int buttonHeight: 40
    property int horizontalPadding: 12
    property color backgroundColor: "#444444"
    property color textColor: "white"
    signal clicked
    implicitWidth: buttonLabel.implicitWidth + horizontalPadding * 2
    width: implicitWidth
    height: buttonHeight
    color: backgroundColor

    Text {
        id: buttonLabel

        anchors.centerIn: parent
        text: parent.text
        color: parent.textColor
        font.family: Theme.fontFamily
        font.pixelSize: Theme.fontSizeMedium
        font.weight: Font.Medium
    }
}
