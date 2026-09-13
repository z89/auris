import QtQuick

Binding {
    property var popout: null
    readonly property var dismissWindow: popout?.backgroundWindow ?? null

    // The invisible DMS dismiss surface can suspend Qt's window polishing.
    // Then its native input mask retains the collapsed hole even though the
    // QML hole has grown, and clicks on setup go to the outside-click handler.
    // Keep polishing enabled for this popup's lifetime; restore DMS's original
    // binding on close, destruction, or a change to a shared-window backend.
    target: dismissWindow
    property: "updatesEnabled"
    value: true
    when: !!dismissWindow && dismissWindow !== popout?.contentWindow && (popout?.shouldBeVisible || popout?.isClosing || false)
    restoreMode: Binding.RestoreBindingOrValue
}
