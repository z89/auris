import QtQuick

QtObject {
    property string path: ""
    property bool connected: false
    property QtObject parser: null
    signal connectionStateChanged
    onConnectedChanged: connectionStateChanged()
    function write(value) {}
    function flush() {}
}
