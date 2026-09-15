import QtCore
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as QQC
import Quickshell
import Quickshell.Io
import qs.Common
import qs.Services
import qs.Widgets
import qs.Modules.Plugins

// auris: AirPods battery per bud and case, ear detection and noise control.
//
// Live data comes from an aurisd control-socket subscription, with
// $XDG_RUNTIME_DIR/aurisd/state.json as a fallback. Control goes the other way
// through the `auris` CLI, which talks to the same daemon socket.
// The plugin owns no state of its own beyond the five options on its settings page.
PluginComponent {
    id: root

    // Bump on development deployments: a reload acknowledgement alone does
    // not prove the live widget was recreated from the current source.
    readonly property string uiRevision: "2026-09-16.1-panel-auto-close"
    Component.onCompleted: console.info("auris: UI loaded revision", uiRevision)
    Component.onDestruction: console.info("auris: UI unloaded revision", uiRevision)

    property var popoutService: null

    // ---- settings ----------------------------------------------------------

    readonly property bool showPercent: pluginData.showPercent !== undefined ? pluginData.showPercent : true
    readonly property string pillValue: pluginData.pillValue !== undefined ? pluginData.pillValue : "buds"
    readonly property int lowThreshold: pluginData.lowThreshold !== undefined ? pluginData.lowThreshold : 20
    readonly property int criticalThreshold: pluginData.criticalThreshold !== undefined ? pluginData.criticalThreshold : 10
    readonly property bool hideWhenDisconnected: pluginData.hideWhenDisconnected !== undefined ? pluginData.hideWhenDisconnected : true

    // Escape hatch for testing and for non-standard install prefixes: the CLI is
    // looked up on PATH unless pluginData.ctlCommand names something else.
    readonly property string ctlCommand: pluginData.ctlCommand ? pluginData.ctlCommand : "auris"

    // ---- data layer --------------------------------------------------------

    readonly property string runtimeDir: {
        const env = Quickshell.env("XDG_RUNTIME_DIR");
        if (env)
            return env;
        const loc = String(StandardPaths.writableLocation(StandardPaths.RuntimeLocation) || "");
        return loc.startsWith("file://") ? loc.substring(7) : loc;
    }
    readonly property string statePath: runtimeDir ? runtimeDir + "/aurisd/state.json" : ""

    // Last successfully parsed state.json, kept across failures so a dropped
    // daemon leaves the last known battery on screen rather than blanking it.
    property var st: null
    property bool daemonUp: false
    property bool socketStreaming: false

    // 1 Hz heartbeat, so "Updated N s ago" moves on its own between file writes.
    property int tick: 0

    // Optimistic echo of a noise mode we asked for but have not seen confirmed.
    // Cleared when the daemon echoes the same mode back, or by pendingNoiseTimeout
    // if the accessory never confirms it.
    property string pendingNoise: ""

    // Requested settings deliberately live outside `st`: a successful CLI exit
    // means the request was sent, never that the accessory applied it. Entries
    // are independent by key so editing microphone never blocks a rename.
    property var pendingAdvanced: ({})
    property int pendingAdvancedSerial: 0
    // Keys whose daemon status turned "confirmed", against the moment it did.
    // The caption fades about four seconds later; the control keeps the value,
    // which by then is the device's own.
    property var settingConfirmedAt: ({})
    property int confirmedFadeTick: 0
    property string advancedStatusKind: ""
    property string advancedStatusText: ""
    property string advancedToastKind: ""
    property string advancedToastText: ""
    property string queuedToastKind: ""
    property string queuedToastText: ""
    property int advancedToastRemaining: 0

    function valuesEqual(left, right) {
        return JSON.stringify(left) === JSON.stringify(right);
    }

    // The daemon publishes a rename under the key "name", in the same
    // settings_requested/settings_status maps it uses for every other write it
    // verifies. This panel has always called that control "rename".
    function daemonKeyOf(key) {
        return key === "rename" ? "name" : key;
    }

    function uiKeyOf(key) {
        return key === "name" ? "rename" : key;
    }

    function pendingAdvancedFor(key) {
        return pendingAdvanced && pendingAdvanced[key] ? pendingAdvanced[key] : null;
    }

    function setAdvancedPending(key, entry) {
        const next = {};
        for (const existingKey of Object.keys(pendingAdvanced || {}))
            next[existingKey] = pendingAdvanced[existingKey];
        if (entry)
            next[key] = entry;
        else
            delete next[key];
        pendingAdvanced = next;
    }

    function clearAdvancedPending(key) {
        if (key === undefined || key === null) {
            pendingAdvanced = {};
            return;
        }
        setAdvancedPending(key, null);
    }

    function showAdvancedToast(kind, message) {
        advancedStatusKind = kind;
        advancedStatusText = message;
        if (advancedToastText.length > 0) {
            queuedToastKind = kind;
            queuedToastText = message;
            return;
        }
        advancedToastKind = kind;
        advancedToastText = message;
        advancedToastRemaining = 50;
        advancedToastTimer.restart();
    }

    function advancedToastBackground(kind) {
        const base = Theme.surfaceContainerHigh;
        const accent = advancedToastAccent(kind);
        // An overlay must obscure the underlying settings, not blend their
        // labels into its message. Tint an opaque surface, not the window.
        return Qt.rgba(base.r * 0.8 + accent.r * 0.2, base.g * 0.8 + accent.g * 0.2, base.b * 0.8 + accent.b * 0.2, 1);
    }

    function dismissAdvancedToast() {
        advancedToastText = "";
        advancedToastKind = "";
        queuedToastText = "";
        queuedToastKind = "";
        advancedToastRemaining = 0;
        advancedToastTimer.stop();
    }

    function advancedToastAccent(kind) {
        if (kind === "error")
            return Theme.error;
        if (kind === "warning")
            return Theme.warning;
        if (kind === "success")
            return Theme.success;
        return Theme.primary;
    }

    function cancelAdvancedPending(reason) {
        if (Object.keys(pendingAdvanced || {}).length === 0)
            return;
        pendingAdvancedSerial++;
        clearAdvancedPending();
        showAdvancedToast("error", reason);
    }

    // From settings_api 2 the daemon verifies its own writes: a settings
    // request lives in settings_requested with a status of its own, so the local
    // entry exists only to serialise commands and is retired as soon as the
    // daemon has taken the value over. From settings_api 3 a rename is one of
    // them, verified from the accessory's own metadata. Older daemons report a
    // rename only as a changed device name, whenever BlueZ next reads one, so
    // that match stays here as a fallback.
    function confirmAdvancedSnapshot(obj) {
        const statuses = obj && obj.settings_status && typeof obj.settings_status === "object" ? obj.settings_status : null;
        const requested = obj && obj.settings_requested && typeof obj.settings_requested === "object" ? obj.settings_requested : null;
        for (const key of Object.keys(pendingAdvanced || {})) {
            const request = pendingAdvanced[key];
            if (!request || request.queued || request.state === "sending")
                continue;
            const daemonKey = daemonKeyOf(key);
            if (statuses && typeof statuses[daemonKey] === "string" && statuses[daemonKey].length > 0 && requested && advancedTargetsEqual(key, requested[daemonKey], request.target)) {
                console.info("auris: setting handed to daemon verification", key, statuses[daemonKey]);
                clearAdvancedPending(key);
                if (key === "rename" && statuses[daemonKey] === "confirmed")
                    showAdvancedToast("success", "Confirmed by the AirPods.");
                continue;
            }
            if (key === "rename" && obj.device && obj.device.name === request.target) {
                console.info("auris: rename echoed by the device");
                clearAdvancedPending(key);
                showAdvancedToast("success", "Confirmed by the AirPods.");
            }
        }
    }

    // Remember when each key first appeared as confirmed, so the caption can
    // fade on its own. Called with the incoming snapshot while `st` is still the
    // previous one, which is what makes the transition observable.
    function noteConfirmedStatuses(obj) {
        const statuses = obj && obj.settings_status && typeof obj.settings_status === "object" ? obj.settings_status : null;
        const previous = settingConfirmedAt || {};
        const now = Date.now();
        const next = {};
        if (statuses) {
            for (const key of Object.keys(statuses)) {
                if (statuses[key] !== "confirmed")
                    continue;
                const wasConfirmed = settingsStatus !== null && settingsStatus[key] === "confirmed";
                next[key] = wasConfirmed && typeof previous[key] === "number" ? previous[key] : now;
            }
        }
        settingConfirmedAt = next;
    }

    // The daemon only tells us when a switch happened. A caption belongs to
    // the moment `at` changes; an event already present on the first snapshot
    // after load is history unless it is only seconds old.
    function noteHandoffEvent(obj, hadState) {
        const handoffObj = obj.handoff && typeof obj.handoff === "object" ? obj.handoff : null;
        const event = handoffObj && handoffObj.last_event && typeof handoffObj.last_event === "object" ? handoffObj.last_event : null;
        const at = event && typeof event.at === "string" ? event.at : "";
        if (at === handoffEventAt)
            return;
        handoffEventAt = at;
        if (at.length === 0)
            return;
        const age = Date.now() - Date.parse(at);
        if (!hadState && !(Math.abs(age) < handoffCaptionMs))
            return;
        handoffEventKind = typeof event.kind === "string" ? event.kind : "";
        handoffCaptionTimer.restart();
    }

    // Audio moved to another Apple device: A2DP may drop while the AAP link
    // stays up. That is not a disconnect for anything on screen.
    function handedOffSnapshot(obj) {
        return !!obj && !!obj.device && obj.device.aap_link === true && !!obj.handoff && typeof obj.handoff === "object" && obj.handoff.owner === "other";
    }

    function parseState(content) {
        if (!content) {
            daemonUp = false;
            cancelAdvancedPending("Request cancelled because aurisd became unavailable.");
            return false;
        }
        try {
            const obj = JSON.parse(content);
            // The control socket answers a bad request with {"ok":false,...} on
            // the same connection, so require the snapshot's schema marker
            // rather than accepting any object.
            if (!obj || typeof obj !== "object" || obj.schema === undefined)
                return false;
            const hadState = st !== null;
            const oldAddress = dev && typeof dev.address === "string" ? dev.address : "";
            const oldModelId = dev && typeof dev.model_id === "string" ? dev.model_id : "";
            const oldAapLink = dev !== null && dev.aap_link === true;
            const oldSettingsApi = settingsApi;
            const newDevice = obj.device || null;
            const newAddress = newDevice && typeof newDevice.address === "string" ? newDevice.address : "";
            const newModelId = newDevice && typeof newDevice.model_id === "string" ? newDevice.model_id : "";
            const newAapLink = newDevice !== null && newDevice.aap_link === true;
            const newSettingsApi = typeof obj.settings_api === "number" ? obj.settings_api : 0;
            const sessionChanged = hadState && (oldAddress !== newAddress || oldModelId !== newModelId || oldAapLink !== newAapLink || oldSettingsApi !== newSettingsApi);
            noteConfirmedStatuses(obj);
            st = obj;
            daemonUp = true;
            noteHandoffEvent(obj, hadState);
            if (sessionChanged && Object.keys(pendingAdvanced || {}).length === 0) {
                advancedStatusKind = "";
                advancedStatusText = "";
            }
            if (pendingNoise && obj.noise_control === pendingNoise) {
                pendingNoise = "";
                pendingNoiseTimeout.stop();
            }
            // The daemon drops and reopens the AAP link to read a write back.
            // connected and aap_link both flick false for about a second while it
            // does; that is a verification step, not a disconnect, and nothing
            // pending may be cancelled because of it.
            // A scheduled reconnect is the same kind of pause: the daemon is
            // holding the link open on the user's behalf, so a pending write
            // waits for the answer rather than being cancelled.
            const reconnecting = obj.link !== null && typeof obj.link === "object" && obj.link.status === "reconnecting";
            if (obj.verify_reopen === true || reconnecting) {
                confirmAdvancedSnapshot(obj);
            } else if (!obj.device || (obj.device.connected !== true && !handedOffSnapshot(obj))) {
                cancelAdvancedPending("Request cancelled because the AirPods disconnected.");
            } else if (obj.settings_api === undefined || obj.settings_api < 1) {
                cancelAdvancedPending("Request cancelled because this aurisd does not support device settings.");
            } else if (obj.device.model_id !== "201B") {
                cancelAdvancedPending("Request cancelled because the connected device changed.");
            } else if (obj.device.aap_link !== true) {
                cancelAdvancedPending("Request cancelled because the AirPods settings link closed.");
            } else {
                for (const key of Object.keys(pendingAdvanced || {})) {
                    const request = pendingAdvanced[key];
                    if (request && request.address.length > 0 && obj.device.address !== request.address) {
                        cancelAdvancedPending("Request cancelled because the connected device changed.");
                        break;
                    }
                }
                confirmAdvancedSnapshot(obj);
            }
            return true;
        } catch (e) {
            // Torn read of a file being replaced under us. Keep the old state;
            // the next push, or the fallback poll, brings the whole thing along.
            return false;
        }
    }

    FileView {
        id: stateFile

        path: root.statePath
        blockWrites: true
        watchChanges: true
        printErrors: false
        onLoaded: {
            if (!root.socketStreaming)
                root.parseState(text());
        }
        onLoadFailed: error => {
            if (!root.socketStreaming) {
                root.daemonUp = false;
                root.cancelAdvancedPending("Request cancelled because aurisd became unavailable.");
            }
        }
    }

    // The daemon pushes a snapshot down its control socket on every change, so
    // the bar appears and disappears in step with the buds instead of up to a
    // poll interval later. One line of JSON per snapshot, same shape as the file.
    Socket {
        id: stateSocket

        path: root.runtimeDir ? root.runtimeDir + "/aurisd/ctl.sock" : ""
        parser: SplitParser {
            splitMarker: "\n"
            onRead: line => {
                if (root.parseState(line))
                    root.socketStreaming = true;
            }
        }
        onConnectionStateChanged: {
            if (connected) {
                write('{"cmd":"subscribe"}\n');
                flush();
            } else {
                root.socketStreaming = false;
                root.daemonUp = false;
                root.cancelAdvancedPending("Request cancelled because aurisd became unavailable.");
            }
        }
    }

    // Dial the socket, and keep dialing while it is down so a daemon restart is
    // picked up. triggeredOnStart makes the first attempt immediate; `running`
    // goes false the moment the stream is up, so nothing ticks in the steady state.
    Timer {
        interval: 2000
        repeat: true
        triggeredOnStart: true
        running: stateSocket.path !== "" && !stateSocket.connected
        onTriggered: stateSocket.connected = true
    }

    // Fallback only. The daemon replaces state.json with an atomic rename, which
    // the file watcher does not always follow, so poll while the push stream is
    // down. The file is under 1 KiB.
    Timer {
        interval: 3000
        repeat: true
        running: root.statePath !== "" && !root.socketStreaming
        onTriggered: stateFile.reload()
    }

    // A command acknowledgement can arrive before writer.rs has finished its
    // 100 ms debounced state-file write. Reloading immediately races that write
    // and can replace a fresh socket push with the previous value. The socket is
    // authoritative while it is connected; this delayed refresh exists only
    // for the file fallback.
    Timer {
        id: fallbackCommandRefresh

        interval: 250
        repeat: false
        onTriggered: stateFile.reload()
    }

    // Only while something is on screen to age: hidden, there is nothing to
    // relabel, and the widget should cost nothing while the buds are away.
    Timer {
        interval: 1000
        repeat: true
        running: root.wantVisible
        onTriggered: root.tick = (root.tick + 1) % 86400
    }

    // The accessory usually echoes a mode change within a few hundred ms. If it
    // never does, drop the optimistic value silently rather than lying forever.
    Timer {
        id: pendingNoiseTimeout

        interval: 5000
        repeat: false
        onTriggered: root.pendingNoise = ""
    }

    // The "Confirmed" caption is transient. Tick while one is on screen so the
    // captions can retire themselves; the binding stops the timer once the last
    // one has aged out, so an idle panel costs nothing.
    Timer {
        id: confirmedFadeTimer

        interval: 250
        repeat: true
        running: root.anyConfirmedCaption
        onTriggered: root.confirmedFadeTick = (root.confirmedFadeTick + 1) % 100000
    }

    Timer {
        id: handoffCaptionTimer

        interval: root.handoffCaptionMs
        repeat: false
        onTriggered: root.handoffEventKind = ""
    }

    Timer {
        id: advancedToastTimer

        interval: 100
        repeat: false
        onTriggered: {
            root.advancedToastRemaining--;
            if (root.advancedToastRemaining > 0) {
                restart();
                return;
            }
            root.advancedToastText = "";
            root.advancedToastKind = "";
            if (root.queuedToastText.length > 0) {
                root.advancedToastKind = root.queuedToastKind;
                root.advancedToastText = root.queuedToastText;
                root.queuedToastKind = "";
                root.queuedToastText = "";
                root.advancedToastRemaining = 50;
                restart();
            }
        }
    }

    // ---- derived state -----------------------------------------------------

    readonly property var dev: st && st.device ? st.device : null
    readonly property var bat: st && st.battery ? st.battery : null
    readonly property var ear: st && st.ear ? st.ear : null

    // ---- link healing ------------------------------------------------------
    //
    // The daemon publishes `link` from the moment it schedules a reconnect
    // until the link is back or it gives up. Older daemons omit the object
    // entirely, and every property below then reads as it did before it
    // existed, so nothing on screen changes for them.
    readonly property var link: st && st.link && typeof st.link === "object" ? st.link : null
    readonly property string linkStatus: link && typeof link.status === "string" ? link.status : ""
    readonly property bool linkReconnecting: linkStatus === "reconnecting"
    readonly property string linkReason: link && typeof link.reason === "string" ? link.reason : ""
    readonly property int linkAttempt: link && typeof link.attempt === "number" ? link.attempt : 0
    // One line, only while the daemon is actually healing the link. The
    // daemon's reason is an inference from Bluetooth events rather than
    // something it is told, and printing it stated a cause nobody had
    // established: buds put back in the case came up as "taken over". The
    // caption now says only what is known, and the attempt count is worth
    // reading only once a first try has already failed. linkReason stays
    // readable for diagnostics, but nothing on screen is derived from it.
    readonly property string linkStatusText: {
        if (!linkReconnecting)
            return "";
        const base = "Attempting to reconnect";
        return linkAttempt > 1 ? base + " \u00b7 attempt " + linkAttempt : base;
    }
    // A disconnect nobody has explained yet. The daemon can take a moment to
    // decide that a dropped link is worth rejoining, and the module blinking
    // out and back in that window reads as the AirPods having gone. Hold it on
    // screen for two seconds; a reconnect that is announced keeps it up through
    // linkReconnecting instead.
    property bool disconnectGrace: false
    readonly property int disconnectGraceMs: 2000

    onConnectedChanged: {
        if (connected) {
            disconnectGrace = false;
            disconnectGraceTimer.stop();
        } else if (daemonUp) {
            disconnectGrace = true;
            disconnectGraceTimer.restart();
        }
    }

    Timer {
        id: disconnectGraceTimer
        interval: root.disconnectGraceMs
        repeat: false
        onTriggered: root.disconnectGrace = false
    }

    // Raw device connection. The daemon briefly reopens the AAP link to read a
    // settings write back, and reports connected/aap_link false for about a
    // second while it does. Nothing on screen may treat that as a disconnect, so
    // `connected` folds the reopen window back in and everything else uses it.
    readonly property bool deviceConnected: dev !== null && dev.connected === true
    readonly property bool verifyReopen: st !== null && st.verify_reopen === true
    readonly property bool connected: deviceConnected || verifyReopen || handoffHeldElsewhere || linkReconnecting
    readonly property string deviceName: dev && dev.name ? dev.name : "AirPods"
    readonly property string model: dev && dev.model ? dev.model : ""
    readonly property string firmware: dev && dev.firmware ? dev.firmware : ""
    readonly property string source: st && st.daemon && st.daemon.source ? st.daemon.source : "none"

    readonly property int settingsApi: st && typeof st.settings_api === "number" ? st.settings_api : 0
    readonly property var confirmedSettings: st && st.settings && typeof st.settings === "object" ? st.settings : null
    readonly property var settingsRequested: st && st.settings_requested && typeof st.settings_requested === "object" ? st.settings_requested : null
    readonly property var settingsStatus: st && st.settings_status && typeof st.settings_status === "object" ? st.settings_status : null
    readonly property string settingsVerify: st && typeof st.settings_verify === "string" ? st.settings_verify : "idle"
    // settings_api 2 is the first daemon that verifies its own writes. Older
    // ones report device values only, and this plugin then says nothing about
    // whether a write landed rather than guessing from a timeout.
    readonly property bool settingsVerified: settingsApi >= 2
    // settings_api 3 is the first daemon that verifies a rename from the
    // accessory's own metadata instead of leaving it to the BlueZ name, which
    // only refreshes on the next connection.
    readonly property bool renameVerified: settingsApi >= 3
    readonly property int confirmedCaptionMs: 4000
    readonly property bool advancedTargetModel: dev !== null && dev.model_id === "201B"
    readonly property bool advancedAapLinked: dev !== null && (dev.aap_link === true || verifyReopen)
    readonly property bool advancedAvailable: daemonUp && connected && settingsApi >= 1 && advancedTargetModel && advancedAapLinked
    readonly property string confirmedDeviceName: dev && typeof dev.name === "string" ? dev.name : ""

    // ---- seamless switching ------------------------------------------------
    //
    // Older daemons omit `handoff`; everything below then stays inert.
    readonly property var handoff: st && st.handoff && typeof st.handoff === "object" ? st.handoff : null
    readonly property bool handoffAvailable: daemonUp && handoff !== null
    readonly property bool handoffHeldElsewhere: handedOffSnapshot(st)
    readonly property var handoffSource: handoff && handoff.audio_source && typeof handoff.audio_source === "object" ? handoff.audio_source : null
    // Each binding guards its own inputs: Qt may evaluate one before the
    // property it reads has caught up with a new snapshot.
    readonly property bool handoffRemotePlaying: daemonUp && handoff !== null && handoff.owner !== "local" && handoffSource !== null && handoffSource.is_local === false && (handoffSource.state === "media" || handoffSource.state === "call")
    readonly property bool handoffRemoteCall: handoffRemotePlaying && handoffSource !== null && handoffSource.state === "call"
    readonly property int handoffCaptionMs: 4000
    property string handoffEventAt: ""
    property string handoffEventKind: ""
    readonly property string handoffCaption: {
        if (!handoffAvailable)
            return "";
        switch (handoffEventKind) {
        case "yielded":
            return "Moved to your other device";
        case "took_over":
            return "Moved here";
        case "yield_requested":
            return "Handing off\u2026";
        default:
            return "";
        }
    }
    readonly property string handoffRowText: handoffCaption.length > 0 ? handoffCaption : !handoffRemotePlaying ? "" : handoffRemoteCall ? "On a call on another device" : "Playing on another device"
    readonly property string handoffRowIcon: {
        switch (handoffCaption.length > 0 ? handoffEventKind : "") {
        case "yielded":
            return "phonelink";
        case "took_over":
            return "headphones";
        case "yield_requested":
            return "swap_horiz";
        default:
            return handoffRemoteCall ? "call" : "devices";
        }
    }
    readonly property bool handoffCardVisible: handoffRowText.length > 0

    function confirmedSetting(key) {
        // The device's own name is the reported value for a rename: the daemon
        // prefers the accessory's metadata name over the BlueZ one.
        if (key === "rename")
            return confirmedDeviceName.length > 0 ? confirmedDeviceName : null;
        if (!confirmedSettings || confirmedSettings[key] === undefined || confirmedSettings[key] === null)
            return null;
        return confirmedSettings[key];
    }

    function keyVerified(key) {
        return key === "rename" ? renameVerified : settingsVerified;
    }

    function settingStatusOf(key) {
        if (!keyVerified(key) || !settingsStatus)
            return "";
        const daemonKey = daemonKeyOf(key);
        return typeof settingsStatus[daemonKey] === "string" ? settingsStatus[daemonKey] : "";
    }

    function requestedSetting(key) {
        if (!keyVerified(key) || !settingsRequested)
            return null;
        const daemonKey = daemonKeyOf(key);
        if (settingsRequested[daemonKey] === undefined || settingsRequested[daemonKey] === null)
            return null;
        return settingsRequested[daemonKey];
    }

    // What a control shows: the value the daemon is carrying for us whenever it
    // has a status for the key, so nothing snaps back to the old value while
    // verification is in flight. A mismatch is the one case where the device
    // won, and its own value is then the honest thing to show.
    function effectiveSetting(key) {
        const status = settingStatusOf(key);
        if (status.length > 0 && status !== "mismatch") {
            const requested = requestedSetting(key);
            if (requested !== null)
                return requested;
        }
        return confirmedSetting(key);
    }

    function confirmedCaptionVisible(key) {
        void confirmedFadeTick;
        const at = settingConfirmedAt || {};
        const daemonKey = daemonKeyOf(key);
        return typeof at[daemonKey] === "number" && Date.now() - at[daemonKey] < confirmedCaptionMs;
    }

    readonly property bool anyConfirmedCaption: {
        void confirmedFadeTick;
        const at = settingConfirmedAt || {};
        const now = Date.now();
        return Object.keys(at).some(key => now - at[key] < confirmedCaptionMs);
    }

    function humaniseSetting(key, value) {
        if (value === null || value === undefined)
            return "its own value";
        if (key === "listening_mode_cycle") {
            const cycle = normalizeListeningCycle(value).map(mode => cycleModeLabel(mode));
            return cycle.length > 0 ? cycle.join(" · ") : "its own cycle";
        }
        if (key === "personalized_volume")
            return value === true ? "On" : value === false ? "Off" : String(value);
        const table = settingValueLabels[key];
        const label = table ? table[String(value)] : undefined;
        return label !== undefined ? label : String(value);
    }

    function cycleModeLabel(mode) {
        switch (mode) {
        case "off":
            return "Off";
        case "anc":
            return "ANC";
        case "transparency":
            return "Transparency";
        case "adaptive":
            return "Adaptive";
        }
        return mode;
    }

    // Same wording the settings component puts on its buttons, so "AirPods kept
    // Slower" names something the user can see on screen.
    readonly property var settingValueLabels: ({
            "microphone": {
                "auto": "Auto",
                "left": "Left",
                "right": "Right"
            },
            "press_speed": {
                "default": "Default",
                "slower": "Slower",
                "slowest": "Slowest"
            },
            "hold_duration": {
                "default": "Default",
                "shorter": "Shorter",
                "shortest": "Shortest"
            },
            "call_controls": {
                "mute_once_hangup_twice": "1\u00d7 mute \u00b7 2\u00d7 end call",
                "hangup_once_mute_twice": "1\u00d7 end call \u00b7 2\u00d7 mute"
            }
        })

    // One caption per control, and only where the daemon has something to say.
    function settingCaptionText(key) {
        switch (settingStatusOf(key)) {
        case "verifying":
            return "Verifying with AirPods\u2026";
        case "confirmed":
            return confirmedCaptionVisible(key) ? "Confirmed" : "";
        case "mismatch":
            return "AirPods kept " + humaniseSetting(key, confirmedSetting(key));
        case "unreported":
            return key === "rename" ? "Sent. The AirPods did not report the name back." : "Applied. AirPods 4 doesn't report this setting back.";
        case "unverified":
            return "Sent, not verified";
        }
        return "";
    }

    function settingCaptionColor(key) {
        const status = settingStatusOf(key);
        if (status === "mismatch")
            return Theme.error;
        if (status === "unverified")
            return Theme.warning;
        return Theme.surfaceVariantText;
    }

    // Captions, keyed by setting: one short line stating what the daemon's own
    // verification found. The settings component renders them under its
    // controls; it is fed this map only when its version understands it, so an
    // older component keeps working and simply shows no caption.
    readonly property var advancedCaptions: {
        void confirmedFadeTick;
        const out = {};
        if (settingsVerified && settingsStatus) {
            for (const daemonKey of Object.keys(settingsStatus)) {
                const key = uiKeyOf(daemonKey);
                const text = settingCaptionText(key);
                if (text.length === 0)
                    continue;
                out[key] = {
                    "text": text,
                    "color": settingCaptionColor(key)
                };
            }
        }
        const local = pendingAdvanced || {};
        for (const key of Object.keys(local)) {
            const request = local[key];
            if (!request || request.state === "sending" || !keyVerified(key))
                continue;
            // Between the command's exit and the daemon's first status for it,
            // the write is already on its way to be verified. Saying so is more
            // accurate than a gap and stops the caption flickering into place.
            if (out[key] === undefined)
                out[key] = {
                    "text": "Verifying with AirPods\u2026",
                    "color": Theme.surfaceVariantText
                };
        }
        return out;
    }

    // The map handed to the settings component as its pendingRequests. It holds
    // only requests this plugin is still carrying itself: a command in flight,
    // and a rename, which the device echoes. Everything the daemon has taken
    // over is absent, because effectiveSetting already puts the right value in
    // front of the control and advancedCaptions says what is happening to it.
    readonly property var advancedRequests: {
        const out = {};
        if (settingsVerified && settingsStatus) {
            for (const daemonKey of Object.keys(settingsStatus)) {
                const key = uiKeyOf(daemonKey);
                if (settingStatusOf(key) !== "verifying")
                    continue;
                const requested = requestedSetting(key);
                if (requested === null)
                    continue;
                out[key] = {
                    "state": "verifying",
                    "target": requested,
                    "deviceValue": confirmedSetting(key)
                };
            }
        }
        const local = pendingAdvanced || {};
        for (const key of Object.keys(local)) {
            const request = local[key];
            if (!request)
                continue;
            if (key === "rename" && !renameVerified) {
                out[key] = {
                    "state": request.state,
                    "target": request.target,
                    "deviceValue": confirmedDeviceName
                };
                continue;
            }
            if (out[key] !== undefined && request.state !== "sending")
                continue;
            out[key] = {
                "state": request.state === "sending" ? "sending" : "verifying",
                "target": request.target,
                "deviceValue": confirmedSetting(key)
            };
        }
        return out;
    }

    readonly property string advancedUnavailableReason: {
        if (!daemonUp)
            return "aurisd is unavailable.";
        if (!connected)
            return "Connect the AirPods to change device settings.";
        if (settingsApi < 1)
            return "Update aurisd to use device settings.";
        if (!advancedTargetModel)
            return "Device settings currently target AirPods 4 ANC (model 201B).";
        if (!advancedAapLinked)
            return "The AirPods settings link is not available.";
        return "";
    }

    readonly property string noise: pendingNoise ? pendingNoise : (st && st.noise_control ? st.noise_control : "unknown")
    readonly property bool caKnown: st !== null && st.conversational_awareness !== null && st.conversational_awareness !== undefined
    readonly property bool ca: caKnown && st.conversational_awareness === true
    readonly property bool adaptiveKnown: st !== null && typeof st.adaptive_level === "number"
    readonly property int adaptiveLevel: adaptiveKnown ? st.adaptive_level : 0

    function slot(side) {
        if (!bat || !bat[side])
            return null;
        return bat[side];
    }
    // Last known level, kept by the daemon even after the component drops
    // out (the buds only relay the case while they sit in it).
    function level(side) {
        const s = slot(side);
        return s && typeof s.level === "number" ? s.level : -1;
    }
    function present(side) {
        const s = slot(side);
        return s !== null && s.present === true;
    }
    function cellSource(side) {
        const s = slot(side);
        return s && typeof s.source === "string" ? s.source : "none";
    }
    function cellFresh(side) {
        const s = slot(side);
        if (!s || !present(side))
            return false;
        // New daemons report freshness per cell. Older snapshots have only a
        // battery-wide stale bit, so retain their established present fallback.
        // A readback reopen costs the daemon its live reports for about a
        // second. Holding the last freshness across that window is what keeps
        // the bar pill from blinking out and back for a verification step.
        if (typeof s.fresh === "boolean")
            return s.fresh || verifyReopen;
        return !(bat && bat.stale === true && !verifyReopen);
    }
    // Level only while the component is reporting right now; the bar pill
    // must not show a number that could be hours old.
    function liveLevel(side) {
        return cellLive(side) ? level(side) : -1;
    }
    function seenCaption(side, heartbeat) {
        void heartbeat;
        const s = slot(side);
        if (!s || !s.last_seen)
            return "not seen yet";
        const historicalCharge = lastKnownCharging(side);
        const state = historicalCharge === true ? " charging" : "";
        const secs = s.last_seen ? Math.round((Date.now() - Date.parse(s.last_seen)) / 1000) : NaN;
        if (!isFinite(secs) || secs < 0)
            return "last known" + state;
        if (secs === 0)
            return "last seen" + state + " just now";
        if (secs < 60)
            return "last seen" + state + " " + secs + " s ago";
        if (secs < 3600)
            return "last seen" + state + " " + Math.round(secs / 60) + " min ago";
        if (secs < 86400)
            return "last seen" + state + " " + Math.round(secs / 3600) + " h ago";
        return "last seen" + state + " " + Math.round(secs / 86400) + " d ago";
    }
    function charging(side) {
        const s = slot(side);
        return s !== null && s.present === true && s.charging === true;
    }
    function earOf(side) {
        return ear && ear[side] ? ear[side] : "unknown";
    }

    // Charging reports can arrive before the in-case ear event. Exclude either
    // signal immediately from the bar, while keeping full data in panel rows.
    function barBudPresent(side) {
        // A BLE battery observation is not an audio connection and must not
        // make the bar look connected by itself.
        return connected && cellLive(side) && !charging(side) && earOf(side) !== "case";
    }

    function cellLive(side) {
        return cellFresh(side);
    }
    function lastKnownCharging(side) {
        const s = slot(side);
        if (!s)
            return null;
        if (typeof s.last_known_charging === "boolean")
            return s.last_known_charging;
        // Old daemons can still supply a current report, but an absent or stale
        // legacy record cannot tell us whether its last reading was charging.
        return cellLive(side) && typeof s.charging === "boolean" ? s.charging : null;
    }
    function panelCharging(side) {
        return cellLive(side) ? charging(side) : lastKnownCharging(side) === true;
    }
    function cellCaption(side, heartbeat) {
        if (!cellLive(side))
            return seenCaption(side, heartbeat);
        const observation = cellSource(side) === "ble" ? "BLE observed · audio link not implied" : "";
        if (side === "case") {
            const status = charging(side) ? "charging" : level(side) === 0 ? "empty" : st && st.lid === "open" ? "lid open" : "";
            return observation && status ? observation + " · " + status : observation || status;
        }
        const inCase = earOf(side) === "case";
        if (charging(side))
            return (observation ? observation + " · " : "") + (inCase ? "in case · charging" : "charging");
        if (inCase)
            return (observation ? observation + " · " : "") + "in case" + (level(side) !== 100 && cellLive("case") && level("case") === 0 ? " · case empty" : "");
        if (earOf(side) === "in")
            return observation;
        return (observation ? observation + " · " : "") + (earOf(side) === "out" ? "out of ear" : "ear status unknown");
    }
    function cellDim(side) {
        // While the link is healing the last reading is still the best one
        // there is, so the row keeps it and says so by dimming instead of
        // blanking out.
        return linkReconnecting || !cellLive(side) || (side !== "case" && !charging(side) && earOf(side) === "out");
    }

    readonly property int leftLevel: barBudPresent("left") ? level("left") : -1
    readonly property int rightLevel: barBudPresent("right") ? level("right") : -1
    readonly property int caseLevel: liveLevel("case")

    readonly property int budsMin: {
        if (leftLevel < 0)
            return rightLevel;
        if (rightLevel < 0)
            return leftLevel;
        return Math.min(leftLevel, rightLevel);
    }
    readonly property int allMin: {
        if (caseLevel < 0)
            return budsMin;
        if (budsMin < 0)
            return caseLevel;
        return Math.min(budsMin, caseLevel);
    }
    readonly property int pillLevel: {
        switch (pillValue) {
        case "all":
            return allMin;
        case "left":
            return leftLevel;
        case "right":
            return rightLevel;
        case "case":
            return caseLevel;
        default:
            return budsMin;
        }
    }

    // Seconds since the daemon last wrote the file, -1 when unknown.
    readonly property int ageSec: {
        tick;
        if (!st || !st.updated_at)
            return -1;
        const t = Date.parse(st.updated_at);
        if (isNaN(t))
            return -1;
        return Math.max(0, Math.round((Date.now() - t) / 1000));
    }

    // Deliberately not a function of ageSec: the daemon only rewrites state.json
    // when a field changes, so a healthy idle link produces an arbitrarily old
    // file. Staleness comes from the daemon's own flag instead.
    readonly property bool stale: !daemonUp || !connected || linkReconnecting || (bat !== null && bat.stale === true && !verifyReopen)

    function ageText() {
        if (ageSec < 0)
            return "never updated";
        if (ageSec < 60)
            return "Updated " + ageSec + " s ago";
        if (ageSec < 3600)
            return "Updated " + Math.round(ageSec / 60) + " min ago";
        return "Updated " + Math.round(ageSec / 3600) + " h ago";
    }

    readonly property string noiseLabel: {
        switch (noise) {
        case "off":
            return "Noise control off";
        case "anc":
            return "ANC";
        case "transparency":
            return "Transparency";
        case "adaptive":
            return "Adaptive";
        default:
            return "Noise control unknown";
        }
    }

    readonly property string statusLine: {
        if (!daemonUp)
            return "aurisd not running";
        if (!connected)
            return ageSec >= 0 ? "Disconnected, last seen " + Math.max(1, Math.round(ageSec / 60)) + " min ago" : "Disconnected";
        return "Connected, " + noiseLabel;
    }

    readonly property string headerNoiseLabel: {
        switch (noise) {
        case "off":
            return "Off";
        case "anc":
            return "ANC";
        case "transparency":
            return "Transparency";
        case "adaptive":
            return "Adaptive";
        default:
            return "Unknown";
        }
    }

    readonly property string headerStatus: {
        if (!daemonUp)
            return "aurisd unavailable";
        if (!connected)
            return "Disconnected";
        return "Connected  ·  " + headerNoiseLabel;
    }

    // ---- colour ------------------------------------------------------------
    //
    // Same ladder as BatteryService.levelColor, but against this plugin's own
    // thresholds. BatteryService itself is the laptop battery and must not be
    // reused here.
    readonly property color cautionColor: "#FFC107"
    readonly property color criticalColor: "#F44336"

    function levelColor(lvl, isCharging) {
        if (isCharging)
            return Theme.success;
        if (lvl < 0)
            return Theme.surfaceVariantText;
        if (lvl <= criticalThreshold)
            return criticalColor;
        if (lvl <= lowThreshold)
            return Theme.warning;
        if (lvl <= lowThreshold * 2)
            return cautionColor;
        return Theme.widgetTextColor;
    }
    function dimmed(c) {
        return stale ? Theme.withAlpha(c, 0.45) : c;
    }

    readonly property color pillColor: dimmed(levelColor(pillLevel, false))
    // Subtle, not an alarm: the module is still showing real readings while
    // the daemon rejoins, and the fade says only that they are being held.
    readonly property real barContentOpacity: linkReconnecting ? 0.55 : 1
    readonly property string pillText: pillLevel >= 0 ? pillLevel + "%" : "--"

    readonly property string noiseIcon: {
        switch (noise) {
        case "anc":
            return "noise_control_on";
        case "transparency":
            return "noise_aware";
        case "adaptive":
            return "blur_on";
        case "off":
            return "noise_control_off";
        default:
            return "headphones";
        }
    }

    // ---- control -----------------------------------------------------------

    readonly property var ctlProcIds: ({
            "noise": "auris.ctl.noise",
            "ca": "auris.ctl.ca",
            "adaptive": "auris.ctl.adaptive",
            "reconnect": "auris.ctl.reconnect"
        })

    function ctl(args) {
        // The shell service may run without ~/.local/bin on PATH; prepend it so a
        // per-user install of aurisd works as well as a system package.
        const argv = ["sh", "-c", "PATH=\"$HOME/.local/bin:$PATH\" exec \"$0\" \"$@\"", ctlCommand].concat(args);
        // Proc.runCommand coalesces calls sharing an id within 50 ms, so each
        // subcommand gets its own id or a quick pair of clicks loses one.
        const procId = ctlProcIds[args[0]] ? ctlProcIds[args[0]] : "auris.ctl." + args[0];
        Proc.runCommand(procId, argv, (stdout, exitCode) => {
            if (exitCode !== 0) {
                root.pendingNoise = "";
                pendingNoiseTimeout.stop();
                ToastService.showError("auris: " + args.join(" ") + " failed", String(stdout || "").trim());
                return;
            }
            if (!root.socketStreaming)
                fallbackCommandRefresh.restart();
        });
    }

    function normalizeListeningCycle(value) {
        const modes = ["off", "anc", "transparency", "adaptive"];
        if (!Array.isArray(value))
            return [];
        return modes.filter(mode => value.indexOf(mode) >= 0);
    }

    function advancedTargetsEqual(key, left, right) {
        if (key === "listening_mode_cycle")
            return valuesEqual(normalizeListeningCycle(left), normalizeListeningCycle(right));
        return valuesEqual(left, right);
    }

    function failAdvancedValidation(message) {
        showAdvancedToast("error", message);
    }

    function startAdvancedCommand(key, request, target, args) {
        const commandSerial = ++pendingAdvancedSerial;
        request.target = target;
        request.args = args;
        request.state = "sending";
        request.inFlightSerial = commandSerial;
        request.sentAt = 0;
        setAdvancedPending(key, request);
        // Deliberately log keys/lifecycle only, never names or preference values.
        console.info("auris: setting command started", key, "request", commandSerial);
        const argv = ["sh", "-c", "PATH=\"$HOME/.local/bin:$PATH\" exec \"$0\" \"$@\"", ctlCommand].concat(args);
        Proc.runCommand("auris.ctl.advanced." + key + "." + commandSerial, argv, (stdout, exitCode) => {
            console.info("auris: setting command completed", key, "request", commandSerial, "exit", exitCode);
            const current = root.pendingAdvancedFor(key);
            if (!current || current.inFlightSerial !== commandSerial)
                return;
            const queued = current.queued;
            if (exitCode !== 0) {
                const detail = String(stdout || "").trim();
                root.showAdvancedToast("error", detail.length > 0 ? detail : "The setting request failed.");
                // A rejected/expired request may signal a changed connection.
                // Do not automatically replay the unsent target into it.
                root.clearAdvancedPending(key);
                return;
            }
            if (queued) {
                current.queued = null;
                root.startAdvancedCommand(key, current, queued.target, queued.args);
                return;
            }
            // This callback has completed. A later click must start a fresh
            // command instead of queuing behind an already-finished process.
            current.inFlightSerial = 0;
            current.state = "awaiting";
            current.sentAt = Date.now();
            // A rename on a daemon that cannot verify one is the exception:
            // there is still the reported name to wait for.
            const awaitsReportedName = key === "rename" && !root.renameVerified;
            if (!root.settingsVerified && !awaitsReportedName) {
                // Pre-verification daemons never report these settings back, so
                // there is nothing to wait for: show the device's own values and
                // say nothing about a write we cannot check.
                root.clearAdvancedPending(key);
                return;
            }
            root.setAdvancedPending(key, current);
            if (key === "rename" && !root.renameVerified)
                root.showAdvancedToast("pending", "Sent; waiting for the AirPods to report the name.");
            if (!root.socketStreaming)
                fallbackCommandRefresh.restart();
        });
    }

    function runAdvancedRequest(key, target, args) {
        if (!advancedAvailable) {
            failAdvancedValidation(advancedUnavailableReason);
            return;
        }
        const current = pendingAdvancedFor(key);
        if (current && current.inFlightSerial) {
            // Proc runs one process per key. A rapid sequence keeps only the
            // latest unsent target, so an older callback cannot overwrite it.
            current.target = target;
            current.args = args;
            current.state = "sending";
            current.queued = {
                "target": target,
                "args": args
            };
            setAdvancedPending(key, current);
            return;
        }
        const request = {
            "target": target,
            "args": args,
            "address": dev && typeof dev.address === "string" ? dev.address : "",
            "state": "sending",
            "inFlightSerial": 0,
            "sentAt": 0,
            "queued": null
        };
        startAdvancedCommand(key, request, target, args);
    }

    function requestAdvancedRename(name) {
        if (!advancedAvailable) {
            failAdvancedValidation(advancedUnavailableReason);
            return;
        }
        if (!pendingAdvancedFor("rename") && name === confirmedDeviceName) {
            showAdvancedToast("success", "That name is already confirmed.");
            return;
        }
        runAdvancedRequest("rename", name, ["rename", name]);
    }

    function requestAdvancedSetting(key, value) {
        if (!advancedAvailable) {
            failAdvancedValidation(advancedUnavailableReason);
            return;
        }
        let target = value;
        if (key === "listening_mode_cycle")
            target = normalizeListeningCycle(value);
        // A rapid revert can legitimately equal the old confirmed value while
        // another requested value is still in flight. Only suppress a true
        // no-op when this key has no outstanding requested target.
        if (!pendingAdvancedFor(key) && advancedTargetsEqual(key, confirmedSetting(key), target)) {
            showAdvancedToast("success", "That value is already confirmed.");
            return;
        }
        runAdvancedRequest(key, target, ["setting", key, JSON.stringify(target)]);
    }

    function setNoise(mode) {
        if (!connected)
            return;
        pendingNoise = mode;
        pendingNoiseTimeout.restart();
        ctl(["noise", mode]);
    }
    function setConversationalAwareness(on) {
        ctl(["ca", on ? "on" : "off"]);
    }
    function setAdaptiveLevel(v) {
        ctl(["adaptive", String(Math.round(v))]);
    }
    function reconnect() {
        ctl(["reconnect"]);
    }
    function setHandoff(on) {
        ctl(["handoff", on ? "on" : "off"]);
    }
    function takeOver() {
        ctl(["take-over"]);
    }
    function yieldAudio() {
        ctl(["yield"]);
    }

    readonly property var noiseModes: ["off", "anc", "transparency", "adaptive"]
    readonly property int noiseIndex: noiseModes.indexOf(noise)

    // ---- visibility --------------------------------------------------------
    //
    // conditionVisible is only consulted when a visibilityCommand is set, so the
    // hide-when-disconnected option goes through the override API instead.
    // Split from wantVisible so the hold logic can be checked without the
    // hide-when-disconnected setting deciding the answer on its own.
    readonly property bool moduleShouldShow: connected || disconnectGrace
    readonly property bool wantVisible: !(hideWhenDisconnected && !moduleShouldShow)

    onWantVisibleChanged: setVisibilityOverride(wantVisible)

    Timer {
        interval: 1
        repeat: false
        running: true
        onTriggered: root.setVisibilityOverride(root.wantVisible)
    }

    // ---- panel auto-close --------------------------------------------------
    //
    // The popout used to outlive the AirPods: the bar module fell back to the
    // plain Bluetooth icon and then hid itself, while an open panel went on
    // showing battery levels and controls for a device that had gone. Close it
    // once the module itself has given up, which is the same moment the bar
    // stops holding the disconnect grace open.
    //
    // A held link is not a disconnect. Healing the link, the settings
    // read-back reopen and audio handed to another host each keep `connected`
    // true, so the panel stays where it is; the explicit term below says so
    // rather than leaving it to `connected` to keep meaning that.
    readonly property bool panelHeldOpen: linkReconnecting || verifyReopen || handoffHeldElsewhere
    readonly property bool panelShouldClose: !panelHeldOpen && !moduleShouldShow
    readonly property bool panelOpen: !!popoutRef && popoutRef.shouldBeVisible === true

    // Only on the transition into the closed-for-good state, never on the
    // panel opening: with hide-when-disconnected off the module stays on the
    // bar, and a panel the user opened then must stay up.
    onPanelShouldCloseChanged: {
        if (panelShouldClose && panelOpen)
            closePopout();
    }

    // ---- bar ---------------------------------------------------------------

    // Show only buds outside the case. If none are known to be available, a
    // neutral Bluetooth icon keeps the panel accessible without inventing buds.
    component PillPodsIcon: Item {
        id: pillPods

        readonly property bool showLeft: root.barBudPresent("left")
        readonly property bool showRight: root.barBudPresent("right")
        readonly property bool showBoth: showLeft && showRight
        readonly property int budSize: showBoth ? Math.max(14, root.iconSize - 3) : root.iconSize
        // The right silhouette ends at .662s and the mirrored left begins at
        // .338s. Overlap their transparent canvases so the *visible* gap is
        // 1.5 px rather than the much larger gap produced by square spacing.
        readonly property real visibleBudGap: 1.5
        readonly property real pairedOffset: budSize * (0.662 - 0.338) + visibleBudGap
        width: showBoth ? pairedOffset + budSize : budSize
        height: root.iconSize

        PodIcon {
            visible: pillPods.showLeft
            // Ear identity, not pair/charging state, determines orientation.
            kind: "left"
            size: pillPods.budSize
            color: root.pillColor
            x: pillPods.showBoth ? pillPods.pairedOffset : (pillPods.width - width) / 2
            anchors.verticalCenter: parent.verticalCenter
        }

        PodIcon {
            visible: pillPods.showRight
            kind: "right"
            size: pillPods.budSize
            color: root.pillColor
            x: pillPods.showBoth ? 0 : (pillPods.width - width) / 2
            anchors.verticalCenter: parent.verticalCenter
        }

        DankIcon {
            visible: !pillPods.showLeft && !pillPods.showRight
            name: "bluetooth_connected"
            size: pillPods.budSize
            color: root.pillColor
            anchors.centerIn: parent
        }
    }

    // Right click flips between ANC and Transparency, the only two modes worth
    // swapping without looking at the panel.
    pillRightClickAction: () => root.setNoise(root.noise === "anc" ? "transparency" : "anc")

    horizontalBarPill: Component {
        Row {
            objectName: "aurisBarPill"
            spacing: Theme.spacingXS
            opacity: root.barContentOpacity

            PillPodsIcon {
                anchors.verticalCenter: parent.verticalCenter
                visible: root.connected && root.daemonUp
            }

            DankIcon {
                anchors.verticalCenter: parent.verticalCenter
                visible: !root.connected || !root.daemonUp
                name: "bluetooth_disabled"
                filled: false
                size: root.iconSize
                color: root.pillColor
            }

            StyledText {
                anchors.verticalCenter: parent.verticalCenter
                visible: root.showPercent && root.pillLevel >= 0
                text: root.pillText
                font.pixelSize: Theme.fontSizeSmall
                color: root.pillColor
            }
        }
    }

    verticalBarPill: Component {
        Column {
            objectName: "aurisBarPillVertical"
            spacing: Theme.spacingXS
            opacity: root.barContentOpacity

            PillPodsIcon {
                anchors.horizontalCenter: parent.horizontalCenter
                visible: root.connected && root.daemonUp
            }

            DankIcon {
                anchors.horizontalCenter: parent.horizontalCenter
                visible: !root.connected || !root.daemonUp
                name: "bluetooth_disabled"
                filled: false
                size: root.iconSize
                color: root.pillColor
            }

            StyledText {
                anchors.horizontalCenter: parent.horizontalCenter
                visible: root.showPercent && root.pillLevel >= 0
                text: String(root.pillLevel)
                font.pixelSize: Theme.fontSizeSmall
                color: root.pillColor
            }
        }
    }

    // ---- shared row --------------------------------------------------------
    //
    // One battery line: icon, label, track, bolt, percentage. DMS ships no
    // progress bar widget, so the track is a Rectangle with a second Rectangle
    // clipped inside it.
    // Original filled bud and case drawings. The base silhouette is the right
    // bud; mirror it for the left everywhere (bar, battery rows and CC).
    component PodIcon: Canvas {
        id: pod

        property string kind: "left"
        property color color: Theme.surfaceText
        property int size: 20

        width: size
        height: size
        antialiasing: true
        onColorChanged: requestPaint()
        onKindChanged: requestPaint()
        onSizeChanged: requestPaint()

        onPaint: {
            const ctx = getContext("2d");
            const s = width;
            ctx.reset();
            ctx.clearRect(0, 0, width, height);
            ctx.fillStyle = pod.color;
            if (pod.kind === "case") {
                const w = s * 0.82, h = s * 0.66, x = (s - w) / 2, y = (s - h) / 2, r = s * 0.17;
                ctx.beginPath();
                ctx.roundedRect(x, y, w, h, r, r);
                ctx.fill();
                ctx.globalCompositeOperation = "destination-out";
                const seam = y + h * 0.4;
                ctx.fillRect(x, seam - s * 0.035, w, s * 0.07);
                ctx.beginPath();
                ctx.arc(s / 2, seam + h * 0.33, s * 0.075, 0, Math.PI * 2);
                ctx.fill();
                ctx.globalCompositeOperation = "source-over";
                return;
            }

            ctx.save();
            if (pod.kind === "left") {
                ctx.translate(s, 0);
                ctx.scale(-1, 1);
            }

            // Original filled silhouette, with the stem shortened from .60s.
            ctx.beginPath();
            ctx.roundedRect(s * 0.47, s * 0.36, s * 0.19, s * 0.48, s * 0.095, s * 0.095);
            ctx.fill();
            ctx.translate(s * 0.4, s * 0.32);
            ctx.rotate(-0.35);
            ctx.beginPath();
            // Qt's ellipse takes a bounding rectangle, not browser radii.
            ctx.ellipse(-s * 0.27, -s * 0.2, s * 0.54, s * 0.4);
            ctx.fill();

            // The speaker opening sits on the listening face; mirroring the
            // whole drawing keeps it on the correct side for each bud.
            ctx.fillStyle = "#20242c";
            ctx.beginPath();
            ctx.ellipse(-s * 0.205, -s * 0.13, s * 0.15, s * 0.26);
            ctx.fill();
            // At bar size the dark oval suggests mesh. Resolve its fine ribs
            // only at larger sizes, where they won't turn into pixel noise.
            if (s >= 32) {
                ctx.save();
                ctx.clip();
                ctx.strokeStyle = "#555d68";
                ctx.lineWidth = s * 0.012;
                for (let row = -2; row <= 2; row++) {
                    ctx.beginPath();
                    ctx.moveTo(-s * 0.205, row * s * 0.045);
                    ctx.lineTo(-s * 0.055, row * s * 0.045);
                    ctx.stroke();
                }
                ctx.restore();
            }
            // Small outer vent, separate from the larger speaker grille.
            ctx.beginPath();
            ctx.ellipse(s * 0.095, -s * 0.05, s * 0.075, s * 0.1);
            ctx.fill();
            ctx.restore();
        }
    }

    component BatteryRow: Item {
        id: batteryRow
        objectName: "battery" + label

        property string label: ""
        property string iconKind: "left"
        property int level: -1
        property bool charging: false
        property string caption: ""
        property bool dim: false

        readonly property bool hasCaption: caption.length > 0

        // Presence, freshness and the expected AAP readback reconnect can all
        // add or clear a caption. Reserve its line permanently: status changes
        // may alter text and tint, never battery-card or panel geometry.
        height: 50
        opacity: dim ? 0.5 : 1

        Behavior on opacity {
            NumberAnimation {
                duration: Theme.shortDuration
                easing.type: Theme.standardEasing
            }
        }

        Item {
            id: line

            anchors.left: parent.left
            anchors.right: parent.right
            anchors.top: parent.top
            height: 34

            PodIcon {
                id: rowIcon

                anchors.left: parent.left
                anchors.verticalCenter: parent.verticalCenter
                kind: batteryRow.iconKind
                size: Theme.iconSize - 2
                color: Theme.surfaceText
            }

            StyledText {
                id: rowLabel

                anchors.left: rowIcon.right
                anchors.leftMargin: Theme.spacingM
                anchors.verticalCenter: parent.verticalCenter
                width: 56
                text: batteryRow.label
                font.pixelSize: Theme.fontSizeSmall
                color: Theme.surfaceText
                elide: Text.ElideRight
            }

            FontMetrics {
                id: percentMetrics
                font.family: Theme.fontFamily
                font.pixelSize: Theme.fontSizeSmall
            }

            Item {
                id: reading
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                // Reserve the same reading column in every row, even with no
                // bolt, so the battery tracks never change length on charging.
                width: Math.ceil(percentMetrics.advanceWidth("100%")) + 16
                height: parent.height

                StyledText {
                    id: rowPercent
                    objectName: "batteryPercent"
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    text: batteryRow.level >= 0 ? batteryRow.level + "%" : "--"
                    font.pixelSize: Theme.fontSizeSmall
                    color: root.levelColor(batteryRow.level, batteryRow.charging)
                }

                DankIcon {
                    id: rowBolt
                    objectName: "batteryChargingBolt"
                    anchors.right: rowPercent.left
                    anchors.rightMargin: 4
                    anchors.verticalCenter: parent.verticalCenter
                    visible: batteryRow.charging
                    name: "bolt"
                    filled: true
                    size: 12
                    color: Theme.success
                }
            }

            Rectangle {
                id: rowTrack
                objectName: "batteryTrack"

                anchors.left: rowLabel.right
                anchors.leftMargin: Theme.spacingL
                anchors.right: reading.left
                anchors.rightMargin: Theme.spacingL
                anchors.verticalCenter: parent.verticalCenter
                height: 7
                radius: height / 2
                color: Theme.withAlpha(Theme.surfaceVariantText, 0.22)

                Rectangle {
                    width: batteryRow.level >= 0 ? Math.max(parent.height, parent.width * batteryRow.level / 100) : 0
                    height: parent.height
                    radius: parent.radius
                    color: root.levelColor(batteryRow.level, batteryRow.charging)

                    Behavior on width {
                        NumberAnimation {
                            duration: Theme.mediumDuration
                            easing.type: Theme.standardEasing
                        }
                    }
                }
            }
        }

        // Second line, under the label and bar, so it never crowds the numbers
        StyledText {
            anchors.left: parent.left
            anchors.leftMargin: rowIcon.width + Theme.spacingM
            anchors.right: parent.right
            anchors.top: line.bottom
            height: 16
            opacity: batteryRow.hasCaption ? 1 : 0
            text: batteryRow.caption
            font.pixelSize: Theme.fontSizeSmall - 1
            color: Theme.surfaceVariantText
            elide: Text.ElideRight
        }
    }

    // One track carved into cells, rather than four buttons sized to their own
    // labels. Each cell is its label plus an equal share of the leftover room,
    // so every segment carries the same padding around its text and the two end
    // gutters match by construction. Equal-width cells cannot do that: "Off"
    // swims in its quarter while "Transparency" touches both edges.
    component NoiseSegments: Item {
        id: seg

        readonly property var labels: ["Off", "ANC", "Transparency", "Adaptive"]
        // Gap between the track and the moving fill, on all four sides.
        readonly property real trackPad: 4
        // The least room a label may keep beside it. Below this the share-out
        // has nothing left to give and an even split is the tidier failure.
        readonly property real minPad: Theme.spacingM

        readonly property var cells: {
            const inner = Math.max(0, width - trackPad * 2);
            const natural = labels.map(l => Math.ceil(segMetrics.advanceWidth(l)));
            const used = natural.reduce((a, b) => a + b, 0);
            const share = (inner - used) / labels.length;
            const roomy = share >= minPad * 2;
            const out = [];
            let x = trackPad;
            for (let i = 0; i < labels.length; i++) {
                const w = roomy ? natural[i] + share : inner / labels.length;
                out.push({
                    "x": x,
                    "w": w,
                    "roomy": roomy
                });
                x += w;
            }
            return out;
        }

        // How close a label may come to its cell edge. Once the cells are down
        // to an even split there is no room to spend, so the label is allowed
        // nearer the edge rather than being shrunk to keep a gap it cannot have.
        readonly property real textInset: cells.length > 0 && cells[0].roomy ? minPad : Theme.spacingS

        height: 40
        enabled: root.connected
        opacity: enabled ? 1 : 0.45

        // Measured at the weight the selected label uses, so a cell never has to
        // grow or clip when the selection lands on it.
        FontMetrics {
            id: segMetrics

            font.family: Theme.fontFamily
            font.pixelSize: Theme.fontSizeSmall
            font.weight: Font.Medium
        }

        Behavior on opacity {
            NumberAnimation {
                duration: Theme.shortDuration
                easing.type: Theme.standardEasing
            }
        }

        Rectangle {
            anchors.fill: parent
            radius: height / 2
            color: Theme.withAlpha(Theme.surfaceVariant, 0.45)
            border.color: Theme.outlineMedium
            border.width: Theme.layerOutlineWidth
        }

        // Sliding the fill is what makes this read as one control with a
        // position, instead of four separate buttons.
        Rectangle {
            readonly property var geom: seg.cells[Math.max(0, root.noiseIndex)]

            x: geom ? geom.x : seg.trackPad
            y: seg.trackPad
            width: geom ? geom.w : 0
            height: parent.height - seg.trackPad * 2
            radius: height / 2
            color: Theme.primary
            visible: root.noiseIndex >= 0

            Behavior on x {
                NumberAnimation {
                    duration: Theme.mediumDuration
                    easing.type: Theme.emphasizedEasing
                }
            }

            Behavior on width {
                NumberAnimation {
                    duration: Theme.mediumDuration
                    easing.type: Theme.emphasizedEasing
                }
            }
        }

        Repeater {
            model: seg.labels

            Item {
                id: cell

                required property int index
                required property string modelData

                readonly property bool selected: index === root.noiseIndex
                readonly property var geom: seg.cells[index]

                x: geom ? geom.x : 0
                y: seg.trackPad
                width: geom ? geom.w : 0
                height: seg.height - seg.trackPad * 2

                Rectangle {
                    anchors.fill: parent
                    radius: height / 2
                    color: Theme.withAlpha(Theme.surfaceText, 0.07)
                    visible: cellHover.hovered && !cell.selected
                }

                StyledText {
                    anchors.centerIn: parent
                    width: parent.width - seg.textInset * 2
                    text: cell.modelData
                    horizontalAlignment: Text.AlignHCenter
                    font.pixelSize: Theme.fontSizeSmall
                    font.weight: cell.selected ? Font.Medium : Font.Normal
                    // Only bites if the user runs a large UI font; all four
                    // shrink together, so they stay even.
                    fontSizeMode: Text.HorizontalFit
                    minimumPixelSize: Theme.fontSizeSmall - 2
                    color: cell.selected ? Theme.primaryText : Theme.surfaceVariantText

                    Behavior on color {
                        ColorAnimation {
                            duration: Theme.shortDuration
                        }
                    }
                }

                HoverHandler {
                    id: cellHover

                    enabled: seg.enabled
                    cursorShape: Qt.PointingHandCursor
                }

                TapHandler {
                    enabled: seg.enabled
                    onTapped: root.setNoise(root.noiseModes[cell.index])
                }
            }
        }
    }

    // ---- panel -------------------------------------------------------------

    TextMetrics {
        id: cycleLabelMetrics
        text: "Transparency"
        font.family: Theme.fontFamily
        font.pixelSize: Theme.fontSizeMedium
        font.weight: Font.Medium
    }
    TextMetrics {
        id: callLabelMetrics
        text: "1× end call · 2× mute"
        font: cycleLabelMetrics.font
    }
    // Measure while the plugin is loaded, not when setup is expanded: opening
    // the disclosure must never change the panel width or reflow quick controls.
    readonly property real preferredPanelWidth: Math.ceil(Math.max(420, Math.max(4 * (cycleLabelMetrics.advanceWidth + Theme.spacingM * 2) + Theme.spacingS * 3, 2 * (callLabelMetrics.advanceWidth + Theme.spacingM * 2) + Theme.spacingS) + Theme.spacingL * 2 + Theme.spacingS * 4))
    popoutWidth: Math.min(preferredPanelWidth, Math.max(0, (root.parentScreen?.width ?? 1920) - 32))
    // Only the size before the first layout: the host rebinds this to the
    // content's own height once the panel is loaded.
    popoutHeight: 520

    // Inside the panel cards. A step up from the gap between them, so content
    // sits clearly within its surface instead of against the edge.
    readonly property real cardPad: Theme.spacingL
    readonly property real controlsPadX: Theme.spacingS
    readonly property real controlsPadY: Theme.spacingM

    // The base class keeps its popout object private; PopoutComponent gets a
    // parentPopout reference when it is loaded, so pass it up here.
    property var popoutRef: null

    Loader {
        // Explicit URL also works in an engine with a cached directory listing.
        source: Qt.resolvedUrl("components/PopoutInputUpdates.qml") + "?v=" + root.uiRevision
        onLoaded: item.popout = Qt.binding(() => root.popoutRef)
    }

    // Qt caches directory listings independently of component URLs. Keeping
    // settings in its own directory avoids a stale plugin-root listing when
    // this component is first installed while the shell is already running.
    readonly property url deviceSettingsUrl: Qt.resolvedUrl("components/settings/AurisAdvancedSettings.qml") + "?t=" + Date.now()

    component HelpButton: DankActionButton {
        id: helpButton
        property string helpText: ""
        property bool helpOpen: false
        readonly property var helpPopup: clickTooltip
        buttonSize: 28
        iconSize: 16
        iconName: "info"
        iconColor: Theme.surfaceVariantText
        // Click help is the only tooltip owner. DMS's hover StateLayer leaves
        // an already-open tooltip alive if tooltipText is cleared on click.
        tooltipText: null
        onClicked: helpOpen = !helpOpen
        onVisibleChanged: if (!visible)
            helpOpen = false

        QQC.ToolTip {
            id: clickTooltip
            parent: helpButton
            x: helpButton.width - width
            y: helpButton.height + Theme.spacingXS
            width: 280
            margins: Theme.spacingS
            focus: true
            visible: helpButton.helpOpen && helpButton.visible
            timeout: -1
            onClosed: helpButton.helpOpen = false
            contentItem: Text {
                text: helpButton.helpText
                wrapMode: Text.WordWrap
                color: Theme.surfaceText
                font.pixelSize: Theme.fontSizeSmall
            }
            background: Rectangle {
                radius: Theme.cornerRadius
                color: Theme.surfaceContainerHigh
                border.color: Theme.outlineMedium
                border.width: Theme.layerOutlineWidth
            }
        }
    }

    popoutContent: Component {
        PopoutComponent {
            id: popout
            readonly property real screenSpace: (root.parentScreen ? root.parentScreen.height : 900) - root.barThickness
            // Normally reserve 96px for the shell. On short screens use some
            // of that slack so setup still has a usable viewport; never borrow
            // space from the fixed quick controls or extend beyond the screen.
            readonly property real verticalBudget: Math.max(0, Math.min(screenSpace - 32, Math.max(screenSpace - 96, 80 + quickControls.height + 128)))
            onParentPopoutChanged: {
                root.popoutRef = parentPopout;
                // Keep the fixed native surface used by DMS's own large popouts.
                if (parentPopout && "fullHeightSurface" in parentPopout)
                    parentPopout.fullHeightSurface = true;
            }
            // DMS owns the one height animation. Reveal at its rendered height
            // instead of scaling the content or adding a second animation.
            clip: true
            height: parentPopout && typeof parentPopout.renderedAlignedHeight === "number" ? Math.max(0, parentPopout.renderedAlignedHeight - Theme.spacingS * 2) : implicitHeight

            headerText: ""
            detailsText: ""
            showCloseButton: false

            Connections {
                target: popout.parentPopout
                ignoreUnknownSignals: true

                function onShouldBeVisibleChanged() {
                    if (!popout.parentPopout || !popout.parentPopout.shouldBeVisible) {
                        panelScroll.contentY = 0;
                        technical.expanded = false;
                        adaptiveHelp.helpOpen = false;
                    }
                }
            }

            // Inset to line the cards up with the header text, which the host
            // indents by spacingS. Flush cards under an indented title is the
            // kind of half-pixel mismatch that reads as sloppy without anyone
            // being able to say why.
            RowLayout {
                id: panelHeader
                objectName: "aurisPanelHeader"
                x: Theme.spacingS
                width: parent.width - Theme.spacingS * 2
                height: 40
                spacing: Theme.spacingM

                StyledText {
                    objectName: "aurisPanelTitle"
                    Layout.fillWidth: true
                    Layout.minimumWidth: 0
                    Layout.alignment: Qt.AlignVCenter
                    text: root.model || root.deviceName
                    font.pixelSize: Theme.fontSizeXLarge - 2
                    font.weight: Font.Bold
                    color: Theme.surfaceText
                    elide: Text.ElideRight
                }

                StyledText {
                    objectName: "aurisPanelStatus"
                    // Reserve at least half the text area for the model on
                    // narrow screens; both labels elide instead of overlapping.
                    Layout.maximumWidth: Math.max(0, (panelHeader.width - 36 - panelHeader.spacing * 2) / 2)
                    Layout.preferredWidth: implicitWidth
                    Layout.minimumWidth: 0
                    Layout.alignment: Qt.AlignVCenter
                    text: root.headerStatus
                    horizontalAlignment: Text.AlignRight
                    font.pixelSize: Theme.fontSizeSmall
                    font.weight: Font.Medium
                    color: Theme.surfaceVariantText
                    elide: Text.ElideRight
                }

                DankActionButton {
                    objectName: "aurisPanelClose"
                    Layout.preferredWidth: 36
                    Layout.preferredHeight: 36
                    iconName: "close"
                    iconSize: Theme.iconSizeSmall
                    iconColor: Theme.surfaceVariantText
                    backgroundColor: "transparent"
                    buttonSize: 36
                    onClicked: root.closePopout()
                }
            }

            StyledRect {
                id: quickControls
                objectName: "aurisQuickControls"
                // Also owns the non-positioned toast layer. Its own geometry
                // still comes solely from quickColumn, never the overlay.
                z: 1
                x: Theme.spacingS
                width: parent.width - Theme.spacingS * 2
                height: quickColumn.implicitHeight + root.controlsPadY * 2
                radius: Theme.cornerRadius
                color: Theme.floatingWindowNestedSurface
                border.color: Theme.outlineMedium
                border.width: Theme.layerOutlineWidth

                Column {
                    id: quickColumn
                    x: root.controlsPadX
                    y: root.controlsPadY
                    width: parent.width - root.controlsPadX * 2
                    spacing: Theme.spacingXS

                    NoiseSegments {
                        objectName: "aurisNoiseModes"
                        width: parent.width
                    }

                    DankToggle {
                        objectName: "aurisConversationToggle"
                        width: parent.width
                        height: 36
                        text: "Conversation awareness"
                        checked: root.ca
                        enabled: root.daemonUp && root.connected && root.caKnown
                        onToggled: isChecked => root.setConversationalAwareness(isChecked)
                    }

                    RowLayout {
                        objectName: "aurisAdaptiveControl"
                        width: parent.width
                        visible: root.noise === "adaptive"
                        height: adaptiveSlider.implicitHeight
                        spacing: Theme.spacingXS
                        StyledText {
                            Layout.preferredWidth: 96
                            text: "Adaptive · " + (root.adaptiveKnown ? root.adaptiveLevel + "%" : "—")
                            elide: Text.ElideRight
                            font.pixelSize: Theme.fontSizeSmall
                            color: Theme.surfaceVariantText
                        }
                        DankSlider {
                            id: adaptiveSlider
                            objectName: "aurisAdaptiveSlider"
                            Layout.fillWidth: true
                            minimum: 0
                            maximum: 100
                            step: 5
                            unit: "%"
                            value: root.adaptiveLevel
                            enabled: root.daemonUp && root.connected && root.advancedAapLinked && root.noise === "adaptive"
                            onSliderDragFinished: finalValue => root.setAdaptiveLevel(finalValue)
                        }
                        HelpButton {
                            id: adaptiveHelp
                            objectName: "aurisAdaptiveHelp"
                            helpText: "Adjusts how much surrounding sound Adaptive mode allows. This is not media volume or a measured noise-cancellation percentage. A dash means the value has not been reported. Requires the control link."
                        }
                    }
                }
            }

            DankFlickable {
                id: panelScroll
                objectName: "aurisMainScroll"
                x: Theme.spacingS
                width: parent.width - Theme.spacingS * 2
                // Quick controls never scroll. Only battery rows may need a
                // small viewport on a short screen with setup expanded.
                height: Math.min(contentHeight, Math.max(0, popout.verticalBudget - 40 - quickControls.height - 40 - (technical.expanded ? 128 : 0)))
                contentWidth: width
                contentHeight: mainPage.implicitHeight + Theme.spacingS
                clip: true
                Column {
                    id: mainPage
                    width: panelScroll.width
                    topPadding: Theme.spacingS
                    StyledRect {
                        width: parent.width
                        height: rows.implicitHeight + root.cardPad * 2
                        radius: Theme.cornerRadius
                        color: Theme.floatingWindowNestedSurface
                        border.color: Theme.outlineMedium
                        border.width: Theme.layerOutlineWidth

                        Column {
                            id: rows

                            anchors.fill: parent
                            anchors.margins: root.cardPad
                            spacing: Theme.spacingM

                            BatteryRow {
                                width: parent.width
                                label: "Left"
                                iconKind: "left"
                                level: root.level("left")
                                charging: root.panelCharging("left")
                                caption: root.cellCaption("left", root.tick)
                                dim: root.cellDim("left")
                            }

                            BatteryRow {
                                width: parent.width
                                label: "Right"
                                iconKind: "right"
                                level: root.level("right")
                                charging: root.panelCharging("right")
                                caption: root.cellCaption("right", root.tick)
                                dim: root.cellDim("right")
                            }

                            BatteryRow {
                                width: parent.width
                                label: "Case"
                                iconKind: "case"
                                level: root.level("case")
                                charging: root.panelCharging("case")
                                caption: root.cellCaption("case", root.tick)
                                dim: root.cellDim("case")
                            }
                        }
                    }

                    // Link healing. One line while the daemon rejoins the
                    // AirPods, so the pause is explained rather than looking
                    // like they went away. Older daemons never open it.
                    Item {
                        id: linkSlot
                        objectName: "aurisLinkSlot"
                        width: parent.width
                        height: root.linkStatusText.length > 0 ? linkCard.height + Theme.spacingS : 0
                        visible: height > 0
                        clip: true
                        property string shownText: ""

                        // Keeps its last line while the slot collapses, the
                        // same way the handoff row does.
                        Binding on shownText {
                            when: root.linkStatusText.length > 0
                            value: root.linkStatusText
                            restoreMode: Binding.RestoreNone
                        }
                        Behavior on height {
                            NumberAnimation {
                                duration: Theme.shortDuration
                                easing.type: Theme.standardEasing
                            }
                        }

                        StyledRect {
                            id: linkCard
                            objectName: "aurisLinkCard"
                            y: Theme.spacingS
                            width: parent.width
                            height: linkRow.height + root.cardPad * 2
                            radius: Theme.cornerRadius
                            color: Theme.floatingWindowNestedSurface
                            border.color: Theme.outlineMedium
                            border.width: Theme.layerOutlineWidth

                            Row {
                                id: linkRow
                                x: root.cardPad
                                y: root.cardPad
                                width: parent.width - root.cardPad * 2
                                height: 24
                                spacing: Theme.spacingM

                                DankIcon {
                                    objectName: "aurisLinkIcon"
                                    anchors.verticalCenter: parent.verticalCenter
                                    name: "sync"
                                    size: Theme.iconSizeSmall
                                    color: Theme.surfaceVariantText
                                }
                                StyledText {
                                    objectName: "aurisLinkText"
                                    anchors.verticalCenter: parent.verticalCenter
                                    width: parent.width - Theme.iconSizeSmall - Theme.spacingM
                                    text: linkSlot.shownText
                                    elide: Text.ElideRight
                                    font.pixelSize: Theme.fontSizeSmall
                                    color: Theme.surfaceText
                                }
                            }
                        }
                    }

                    // Seamless switching. The slot animates its own height so
                    // the row eases in instead of shoving the setup chevron,
                    // and keeps its last text while it collapses.
                    Item {
                        id: handoffSlot
                        objectName: "aurisHandoffSlot"
                        width: parent.width
                        height: root.handoffCardVisible ? handoffCard.height + Theme.spacingS : 0
                        visible: height > 0
                        clip: true
                        property string shownText: ""
                        property string shownIcon: "devices"

                        Binding on shownText {
                            when: root.handoffCardVisible
                            value: root.handoffRowText
                            restoreMode: Binding.RestoreNone
                        }
                        Binding on shownIcon {
                            when: root.handoffCardVisible
                            value: root.handoffRowIcon
                            restoreMode: Binding.RestoreNone
                        }
                        Behavior on height {
                            NumberAnimation {
                                duration: Theme.shortDuration
                                easing.type: Theme.standardEasing
                            }
                        }

                        StyledRect {
                            id: handoffCard
                            objectName: "aurisHandoffCard"
                            y: Theme.spacingS
                            width: parent.width
                            height: handoffRow.height + root.cardPad * 2
                            radius: Theme.cornerRadius
                            color: Theme.floatingWindowNestedSurface
                            border.color: Theme.outlineMedium
                            border.width: Theme.layerOutlineWidth

                            RowLayout {
                                id: handoffRow
                                x: root.cardPad
                                y: root.cardPad
                                width: parent.width - root.cardPad * 2 - (handoffUseHere.visible ? handoffUseHere.width + Theme.spacingM : 0)
                                // Fixed so the button coming and going with a
                                // caption never changes the card's height.
                                height: 36
                                spacing: Theme.spacingM

                                DankIcon {
                                    objectName: "aurisHandoffIcon"
                                    Layout.alignment: Qt.AlignVCenter
                                    name: handoffSlot.shownIcon
                                    size: Theme.iconSizeSmall
                                    color: Theme.surfaceVariantText
                                }
                                StyledText {
                                    objectName: "aurisHandoffText"
                                    Layout.fillWidth: true
                                    Layout.minimumWidth: 0
                                    Layout.alignment: Qt.AlignVCenter
                                    text: handoffSlot.shownText
                                    elide: Text.ElideRight
                                    font.pixelSize: Theme.fontSizeSmall
                                    color: Theme.surfaceText
                                }
                            }

                            // Anchored rather than laid out: DankButton sizes
                            // itself through width, which a layout cannot read.
                            DankButton {
                                id: handoffUseHere
                                objectName: "aurisHandoffUseHere"
                                anchors.right: parent.right
                                anchors.rightMargin: root.cardPad
                                y: root.cardPad
                                visible: root.handoffRemotePlaying
                                enabled: root.daemonUp
                                text: "Use here"
                                buttonHeight: 36
                                onClicked: root.takeOver()
                            }
                        }
                    }
                }
            }

            Item {
                id: technical
                objectName: "aurisTechnicalDisclosure"
                property bool expanded: false
                readonly property real feedbackSlotHeight: 52
                x: Theme.spacingS
                width: parent.width - Theme.spacingS * 2
                implicitHeight: 40 + (expanded ? technicalScroll.height + Theme.spacingS : 0)
                height: implicitHeight
                onExpandedChanged: {
                    if (!expanded) {
                        if (deviceSettings.item)
                            deviceSettings.item.activeHelpKey = "";
                        root.dismissAdvancedToast();
                    }
                }

                DankActionButton {
                    objectName: "aurisSetupDisclosureButton"
                    anchors.horizontalCenter: parent.horizontalCenter
                    iconName: technical.expanded ? "keyboard_arrow_up" : "keyboard_arrow_down"
                    buttonSize: 36
                    iconSize: 20
                    iconColor: Theme.surfaceVariantText
                    // The disclosure chevron is self-explanatory; null avoids
                    // a second tooltip owner beside click-only help controls.
                    tooltipText: null
                    onClicked: technical.expanded = !technical.expanded
                }

                DankFlickable {
                    id: technicalScroll
                    objectName: "aurisSetupScroll"
                    y: 40
                    width: parent.width
                    height: Math.min(contentHeight, 480, Math.max(0, popout.verticalBudget - 40 - quickControls.height - panelScroll.height - 40 - Theme.spacingS))
                    // Retain the content during the closing reveal, too. Its
                    // target contribution to implicitHeight is already zero.
                    visible: technical.expanded || popout.height > technical.y + 40 + 0.5
                    enabled: technical.expanded
                    onVisibleChanged: if (!visible)
                        contentY = 0
                    contentWidth: width
                    contentHeight: detailsColumn.implicitHeight
                    clip: true

                    Column {
                        id: detailsColumn
                        width: technicalScroll.width
                        spacing: Theme.spacingM
                        StyledText {
                            text: "AirPods setup"
                            font.pixelSize: Theme.fontSizeMedium
                            font.weight: Font.Medium
                            color: Theme.surfaceText
                        }
                        // URL loading also works when the live engine indexed this
                        // directory before the settings component was installed.
                        Loader {
                            id: deviceSettings
                            objectName: "aurisSettingsLoader"

                            width: parent.width
                            height: item ? item.implicitHeight : 0
                            property int retrySerial: 0
                            source: root.deviceSettingsUrl + "&retry=" + retrySerial
                            function retryLoad() {
                                source = "";
                                retrySerial++;
                                source = root.deviceSettingsUrl + "&retry=" + retrySerial;
                            }
                            function logComponentError() {
                                // Loader exposes a status but no errorString(). A
                                // component probe exposes the underlying QML error.
                                const probe = Qt.createComponent(source, Component.PreferSynchronous);
                                function report() {
                                    if (probe.status === Component.Loading)
                                        return;
                                    console.error("auris: settings component diagnostic", source, probe.status, probe.errorString());
                                    probe.destroy();
                                }
                                if (probe.status === Component.Loading)
                                    probe.statusChanged.connect(report);
                                else
                                    report();
                            }
                            onStatusChanged: {
                                if (status === Loader.Error) {
                                    console.error("auris: device settings failed to load", source);
                                    logComponentError();
                                }
                            }
                            onLoaded: {
                                item.available = Qt.binding(() => root.advancedAvailable);
                                item.unavailableReason = Qt.binding(() => root.advancedUnavailableReason);
                                item.pendingRequests = Qt.binding(() => root.advancedRequests);
                                // Reading an absent property yields undefined
                                // rather than throwing, so a settings component
                                // that predates daemon-verified writes still
                                // loads: it just shows values without captions.
                                if (item.captions !== undefined)
                                    item.captions = Qt.binding(() => root.advancedCaptions);
                                else
                                    console.info("auris: settings component has no caption lane; showing values only");
                                item.deviceIdentity = Qt.binding(() => root.dev && typeof root.dev.address === "string" ? root.dev.address : "");
                                item.deviceName = Qt.binding(() => root.confirmedDeviceName);
                                item.microphone = Qt.binding(() => root.effectiveSetting("microphone"));
                                item.pressSpeed = Qt.binding(() => root.effectiveSetting("press_speed"));
                                item.holdDuration = Qt.binding(() => root.effectiveSetting("hold_duration"));
                                item.listeningModeCycle = Qt.binding(() => root.effectiveSetting("listening_mode_cycle"));
                                item.callControls = Qt.binding(() => root.effectiveSetting("call_controls"));
                                item.personalizedVolume = Qt.binding(() => root.effectiveSetting("personalized_volume"));
                                if (item.handoff !== undefined)
                                    item.handoff = Qt.binding(() => root.handoffAvailable ? root.handoff : null);
                                console.info("auris: device settings loaded", source);
                            }

                            Connections {
                                target: deviceSettings.item
                                ignoreUnknownSignals: true

                                function onHandoffToggleRequested(on) {
                                    root.setHandoff(on);
                                }
                                function onRenameRequested(name) {
                                    root.requestAdvancedRename(name);
                                }
                                function onSettingRequested(key, value) {
                                    root.requestAdvancedSetting(key, value);
                                }
                            }
                        }

                        Column {
                            objectName: "aurisSettingsError"
                            width: parent.width
                            visible: deviceSettings.status !== Loader.Ready
                            spacing: Theme.spacingS

                            StyledText {
                                width: parent.width
                                text: deviceSettings.status === Loader.Error ? "Device settings could not load. Retry below; details are in the shell log." : "Loading device settings…"
                                wrapMode: Text.WordWrap
                                color: Theme.warning
                                font.pixelSize: Theme.fontSizeSmall
                            }
                            DankButton {
                                objectName: "aurisSettingsRetry"
                                width: parent.width
                                visible: deviceSettings.status === Loader.Error
                                text: "Retry device settings"
                                onClicked: deviceSettings.retryLoad()
                            }
                        }

                        StyledRect {
                            id: technicalCard
                            width: parent.width
                            height: technicalColumn.implicitHeight + Theme.spacingL * 2
                            radius: Theme.cornerRadius
                            color: Theme.floatingWindowNestedSurface

                            Column {
                                id: technicalColumn
                                x: Theme.spacingL
                                y: Theme.spacingL
                                width: parent.width - Theme.spacingL * 2
                                spacing: Theme.spacingM

                                StyledText {
                                    text: "Device & connection"
                                    font.pixelSize: Theme.fontSizeMedium
                                    font.weight: Font.Medium
                                    color: Theme.surfaceText
                                }

                                Repeater {
                                    model: [
                                        {
                                            label: "Model",
                                            value: root.model || "Not reported"
                                        },
                                        {
                                            label: "Firmware",
                                            value: root.firmware || "Not reported"
                                        },
                                        {
                                            label: "Bluetooth",
                                            value: !root.daemonUp ? "Unknown (daemon unavailable)" : root.connected ? "Connected" : "Disconnected"
                                        },
                                        {
                                            label: "Control link",
                                            value: root.daemonUp && root.advancedAapLinked ? "Ready (AAP)" : "Unavailable"
                                        },
                                        {
                                            label: "Data source",
                                            value: !root.daemonUp || root.source === "none" ? "No live source" : root.source.toUpperCase()
                                        },
                                        {
                                            label: "Daemon",
                                            value: root.daemonUp && root.st && root.st.daemon ? "aurisd " + root.st.daemon.version : "Unavailable"
                                        },
                                        {
                                            label: "Settings API",
                                            value: root.settingsApi >= 1 ? String(root.settingsApi) : "Not supported"
                                        },
                                        {
                                            label: "UI revision",
                                            value: root.uiRevision
                                        },
                                        {
                                            label: "Last update",
                                            value: root.ageText()
                                        },
                                        {
                                            label: "Settings panel",
                                            value: deviceSettings.status === Loader.Ready ? "Loaded" : deviceSettings.status === Loader.Error ? "Load failed" : "Loading"
                                        }
                                    ]
                                    RowLayout {
                                        required property var modelData
                                        width: technicalColumn.width
                                        spacing: Theme.spacingM
                                        StyledText {
                                            Layout.preferredWidth: 94
                                            Layout.alignment: Qt.AlignTop
                                            text: modelData.label
                                            font.pixelSize: Theme.fontSizeSmall
                                            color: Theme.surfaceVariantText
                                        }
                                        StyledText {
                                            Layout.fillWidth: true
                                            text: modelData.value
                                            wrapMode: Text.WrapAnywhere
                                            font.pixelSize: Theme.fontSizeSmall
                                            color: Theme.surfaceText
                                        }
                                    }
                                }

                                DankButton {
                                    width: parent.width
                                    enabled: root.daemonUp && root.connected
                                    text: "Reconnect control link"
                                    iconName: "refresh"
                                    buttonHeight: 40
                                    onClicked: root.reconnect()
                                }
                                StyledText {
                                    width: parent.width
                                    text: "Reopens Auris’s settings and telemetry link. Does not pair or reconnect Bluetooth audio."
                                    wrapMode: Text.WordWrap
                                    font.pixelSize: Theme.fontSizeSmall - 1
                                    color: Theme.surfaceVariantText
                                }
                            }
                        }

                        // Keeps the final content clear of the fixed feedback
                        // lane when the setup viewport is scrolled to its end.
                        Item {
                            width: 1
                            height: technical.feedbackSlotHeight + Theme.spacingS
                        }
                    }
                }

                // This footer is allocated for the entire time setup is open.
                // Status visibility and text never add or remove layout space.
                Item {
                    id: advancedFeedbackSlot
                    objectName: "aurisAdvancedFeedbackSlot"
                    y: 40 + Math.max(0, technicalScroll.height - height)
                    width: parent.width
                    height: technical.expanded ? technical.feedbackSlotHeight : 0
                    clip: true
                    z: 20

                    StyledRect {
                        id: advancedToast
                        objectName: "aurisAdvancedToast"
                        anchors.fill: parent
                        opacity: root.advancedToastText.length > 0 ? 1 : 0
                        enabled: opacity > 0
                        radius: Theme.cornerRadius
                        // Tint a neutral surface instead of putting surfaceText over a
                        // saturated semantic colour; this remains legible in both DMS
                        // light and dark palettes.
                        color: root.advancedToastBackground(root.advancedToastKind)
                        border.color: root.advancedToastAccent(root.advancedToastKind)
                        border.width: Theme.layerOutlineWidth

                        MouseArea {
                            anchors.fill: parent
                            acceptedButtons: Qt.AllButtons
                            onWheel: event => event.accepted = true
                        }

                        DankIcon {
                            objectName: "aurisAdvancedToastIcon"
                            anchors.left: parent.left
                            anchors.leftMargin: Theme.spacingM
                            anchors.verticalCenter: parent.verticalCenter
                            name: root.advancedToastKind === "error" ? "error" : root.advancedToastKind === "warning" ? "warning" : root.advancedToastKind === "success" ? "check_circle" : "info"
                            size: Theme.iconSizeSmall
                            color: root.advancedToastAccent(root.advancedToastKind)
                        }
                        StyledText {
                            objectName: "aurisAdvancedToastText"
                            anchors.left: parent.left
                            anchors.right: closeToast.left
                            anchors.verticalCenter: parent.verticalCenter
                            anchors.leftMargin: Theme.spacingM * 2 + Theme.iconSizeSmall
                            anchors.rightMargin: Theme.spacingS
                            text: root.advancedToastText
                            elide: Text.ElideRight
                            maximumLineCount: 2
                            wrapMode: Text.WordWrap
                            font.pixelSize: Theme.fontSizeSmall
                            color: Theme.surfaceText
                        }
                        DankActionButton {
                            id: closeToast
                            objectName: "aurisDismissToast"
                            anchors.right: parent.right
                            anchors.rightMargin: Theme.spacingXS
                            anchors.verticalCenter: parent.verticalCenter
                            buttonSize: 28
                            iconSize: 16
                            iconName: "close"
                            iconColor: Theme.surfaceText
                            backgroundColor: "transparent"
                            tooltipText: null
                            onClicked: root.dismissAdvancedToast()
                        }
                        Rectangle {
                            anchors.left: parent.left
                            anchors.bottom: parent.bottom
                            height: 2
                            width: parent.width * Math.max(0, root.advancedToastRemaining) / 50
                            color: Theme.surfaceText
                            opacity: 0.65
                        }
                    }
                }
            }
        }
    }

    // ---- control centre ----------------------------------------------------

    ccWidgetIcon: connected ? noiseIcon : "bluetooth_disabled"
    ccWidgetPrimaryText: "AirPods"
    ccWidgetSecondaryText: {
        if (!daemonUp)
            return "aurisd not running";
        if (!connected)
            return "Disconnected";
        return (pillLevel >= 0 ? pillLevel + "%  ·  " : "") + noiseLabel;
    }
    ccWidgetIsActive: connected && !stale
    ccDetailHeight: 300

    onCcWidgetToggled: {
        if (connected)
            setNoise(noise === "anc" ? "transparency" : "anc");
        else
            reconnect();
    }

    ccDetailContent: Component {
        Rectangle {
            // Fixed, because the host sizes this pane from ccDetailHeight and a
            // Column that fills it cannot also measure it: the two bind in a
            // circle and Qt breaks it by reporting nothing. Keep the two in step.
            implicitHeight: root.ccDetailHeight
            radius: Theme.cornerRadius
            color: Theme.surfaceContainerHigh

            Column {
                anchors.fill: parent
                anchors.margins: Theme.spacingL
                spacing: Theme.spacingM

                Column {
                    width: parent.width
                    spacing: Theme.spacingS

                    BatteryRow {
                        width: parent.width
                        label: "Left"
                        iconKind: "left"
                        level: root.level("left")
                        charging: root.panelCharging("left")
                        caption: root.cellCaption("left", root.tick)
                        dim: root.cellDim("left")
                    }

                    BatteryRow {
                        width: parent.width
                        label: "Right"
                        iconKind: "right"
                        level: root.level("right")
                        charging: root.panelCharging("right")
                        caption: root.cellCaption("right", root.tick)
                        dim: root.cellDim("right")
                    }

                    BatteryRow {
                        width: parent.width
                        label: "Case"
                        iconKind: "case"
                        level: root.level("case")
                        charging: root.panelCharging("case")
                        caption: root.cellCaption("case", root.tick)
                        dim: root.cellDim("case")
                    }
                }

                NoiseSegments {
                    width: parent.width
                }

                StyledText {
                    width: parent.width
                    text: root.statusLine
                    font.pixelSize: Theme.fontSizeSmall
                    color: Theme.surfaceVariantText
                    elide: Text.ElideRight
                }
            }
        }
    }
}
