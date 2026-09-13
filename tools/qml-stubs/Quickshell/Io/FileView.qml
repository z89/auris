import QtQuick

QtObject {
    property string path: ""
    property bool blockWrites: false
    property bool watchChanges: false
    property bool printErrors: false
    signal loaded
    signal loadFailed(var error)
    function text() { return ""; }
    function reload() {}
}
