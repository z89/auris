import QtQuick

Item {
    property real minimum: 0
    property real maximum: 100
    property real step: 1
    property string unit: ""
    property real value: 0
    property string leftIcon: ""
    signal sliderDragFinished(real finalValue)
    implicitHeight: 48
    height: implicitHeight
}
