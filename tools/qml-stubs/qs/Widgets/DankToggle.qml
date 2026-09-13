import QtQuick

Item {
    property string text: ""
    property string description: ""
    property bool checked: false
    signal toggled(bool checked)
    implicitHeight: 40
    height: implicitHeight
}
