pragma Singleton
import QtQuick

QtObject {
    function env(name) {
        return name === "XDG_RUNTIME_DIR" ? "/__auris_headless_no_runtime__" : "";
    }
}
