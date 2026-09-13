pragma Singleton
import QtQuick

QtObject {
    // Default remains the old deterministic failure. Tests opt in to retaining
    // callbacks so they can exercise ordering and stale-callback guards.
    property bool controlled: false
    property var calls: []

    function reset() {
        calls = [];
    }

    function runCommand(id, argv, callback) {
        if (!controlled) {
            if (callback)
                callback("headless command execution is disabled", 1);
            return;
        }
        calls = calls.concat([
            {
                "id": id,
                "argv": argv,
                "callback": callback
            }
        ]);
    }

    function complete(index, stdout, exitCode) {
        if (index < 0 || index >= calls.length)
            return false;
        const callback = calls[index].callback;
        if (callback)
            callback(stdout, exitCode);
        return true;
    }
}
