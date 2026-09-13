import QtQuick

Rectangle {
    id: root

    property alias text: input.text
    property string placeholderText: ""
    property int maximumLength: 32767
    property bool showClearButton: false
    signal accepted

    TextInput {
        id: input
        anchors.fill: parent
        maximumLength: root.maximumLength
        onAccepted: root.accepted()
    }
}
