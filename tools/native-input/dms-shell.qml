import QtQuick
import Quickshell
import qs.Widgets
import AurisInputProbe 1.0

Scope {
    id: test
    property int phase: 0
    property var contentItem: null
    InputProbe { id: probe }
    DankPopoutStandalone {
        id: popup
        fullHeightSurface: true
        popupWidth: 420
        popupHeight: 200
        screen: Quickshell.screens[0]
        triggerX: 280
        triggerY: 52
        content: Component {
            Rectangle {
                color: "#334455"
                Component.onCompleted: test.contentItem = this
                MouseArea {
                    anchors.fill: parent
                    hoverEnabled: true
                    onClicked: console.log("CONTENT CLICK")
                    onWheel: console.log("CONTENT WHEEL")
                }
            }
        }
        onBackgroundClicked: {
            console.log("DISMISS CLICK");
            close();
        }
    }
    Timer {
        interval: 700
        running: true
        repeat: true
        onTriggered: {
            if (test.contentItem) {
                console.log("phase", test.phase, "content", probe.describe(test.contentItem), "background", probe.describe(popup.backgroundWindow.contentItem), "hole", popup._surfaceBodyX, popup._surfaceBodyY, popup._surfaceBodyW, popup._surfaceBodyH, "updates", popup.backgroundWindow.updatesEnabled);
            }
            switch (test.phase++) {
            case 0: popup.open(); break;
            case 1:
                // Reproduce the captured live state: DMS's invisible dismiss
                // window has stopped updating before the disclosure grows.
                popup.backgroundWindow.updatesEnabled = false;
                popup.popupHeight = 650;
                break;
            case 2:
                console.log("expanded hit", probe.accepts(test.contentItem, 200, 550), "dismiss hit", probe.accepts(popup.backgroundWindow.contentItem, 200, 550));
                break;
            case 3: Qt.quit();
            }
        }
    }
}
