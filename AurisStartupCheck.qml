import QtQuick
import qs.Common

// The widget is a view onto aurisd. Without the CLI there is no daemon to talk
// to, so block activation rather than showing a permanently empty pill.
QtObject {
    function check(done) {
        Proc.runCommand("auris.depCheck", ["sh", "-c", "command -v auris >/dev/null 2>&1 || test -x \"$HOME/.local/bin/auris\""], (stdout, exitCode) => {
            if (exitCode === 0) {
                done(null);
                return;
            }
            done({
                "title": "aurisd is required",
                "details": "This widget reads $XDG_RUNTIME_DIR/aurisd/state.json and drives the AirPods through auris. Install the aurisd package, start the aurisd user service, then re-enable this plugin."
            });
        });
    }
}
