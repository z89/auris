import QtQuick
import Quickshell
import AurisInputProbe 1.0

Scope {
    id: test
    property int phase: 0
    property int bodyHeight: 200
    property int clicks: 0
    property bool routingCorrect: true
    InputProbe { id: probe }
    Loader {
        active: Quickshell.env("AURIS_INPUT_FIX") === "1"
        source: "PopoutInputUpdates.qml"
        onLoaded: item.popout = host
    }
    QtObject {
        id: host
        property var backgroundWindow: background
        property var contentWindow: content
        property bool shouldBeVisible: false
        property bool isClosing: false
    }

    FloatingWindow {
        id: background
        visible: true
        implicitWidth: 600
        implicitHeight: 800
        color: "transparent"
        updatesEnabled: true
        mask: Region {
            item: backgroundRect
            Region { item: hole; intersection: Intersection.Subtract }
        }
        Item { id: backgroundRect; width: 600; height: 800 }
        Item { id: hole; x: 40; y: 20; width: 420; height: test.bodyHeight }
    }
    FloatingWindow {
        id: content
        visible: true
        implicitWidth: 500
        implicitHeight: 800
        color: "transparent"
        mask: Region { item: body }
        Rectangle {
            id: body
            x: 40; y: 20; width: 420; height: test.bodyHeight
            color: "#334455"
            MouseArea { anchors.fill: parent; hoverEnabled: true; onClicked: test.clicks++ }
        }
    }
    Timer {
        interval: 350
        running: true
        repeat: true
        onTriggered: {
            console.log("phase", test.phase, "content", probe.describe(body), "background", probe.describe(backgroundRect));
            switch (test.phase++) {
            case 0:
                background.updatesEnabled = false;
                host.shouldBeVisible = true;
                break;
            case 1:
                test.bodyHeight = 650;
                break;
            case 2:
            case 5:
                const expandedCorrect = probe.accepts(body, 200, 550) && !probe.accepts(backgroundRect, 200, 550)
                                     && probe.accepts(body, 200, 100) && !probe.accepts(backgroundRect, 200, 100)
                                     && probe.accepts(backgroundRect, 550, 550);
                console.log("expanded hit", probe.accepts(body, 200, 550), "dismiss hit", probe.accepts(backgroundRect, 200, 550));
                test.routingCorrect = test.routingCorrect && expandedCorrect;
                if (!Quickshell.env("AURIS_NATIVE_WAYLAND") && expandedCorrect)
                    probe.click(body, 200, 550);
                if (test.phase === 6) {
                    host.isClosing = true;
                    host.shouldBeVisible = false;
                    test.routingCorrect = test.routingCorrect && background.updatesEnabled;
                }
                break;
            case 3:
                test.bodyHeight = 200;
                break;
            case 4:
                test.routingCorrect = test.routingCorrect && !probe.accepts(body, 200, 550) && probe.accepts(backgroundRect, 200, 550);
                test.bodyHeight = 650;
                break;
            case 6:
                host.isClosing = false;
                break;
            case 7:
                console.log("RESULT clicks", test.clicks);
                console.log("RESULT restored idle updates", background.updatesEnabled);
                Qt.exit(test.routingCorrect && test.clicks === 2 && !background.updatesEnabled ? 0 : 1);
            }
        }
    }
}
