//! BlueZ side: pick the device, then watch its `Connected` property.
//!
//! This task NEVER initiates a connection. It only reads properties and
//! subscribes to `PropertiesChanged`; the accessory is connected by the user
//! or by BlueZ's own auto-connect.

use std::{collections::HashMap, sync::Arc, time::Duration};

use bluer::{Adapter, Address, Device, DeviceEvent, DeviceProperty, ErrorKind, Session};
use dbus::{
    arg::{prop_cast, PropMap},
    message::MatchRule,
    nonblock::{MsgMatch, Proxy, SyncConnection},
    Message,
};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::{models, rejoin};

/// How long to wait before rebuilding the D-Bus chain after an error.
const REBUILD_DELAY: Duration = Duration::from_secs(2);
/// How long to wait when no matching device is paired yet.
const RESCAN_DELAY: Duration = Duration::from_secs(10);
/// Reconciliation tick: re-read `Connected` in case an event was missed.
const RECONCILE: Duration = Duration::from_secs(30);
/// Longest wait for BlueZ's object list when the audio watch starts.
const MANAGED_OBJECTS_TIMEOUT: Duration = Duration::from_secs(5);
const TRANSPORT_IFACE: &str = "org.bluez.MediaTransport1";
const DEVICE_IFACE: &str = "org.bluez.Device1";

/// Something the BlueZ layer learned.
#[derive(Debug, Clone)]
pub enum LinkEvent {
    /// The device to work with, and the adapter to dial from.
    Identity {
        /// Local adapter address, for binding the L2CAP socket.
        adapter: Address,
        /// Accessory address.
        address: Address,
        /// Bluetooth name, if known.
        name: Option<String>,
        /// Uppercase hex product id from the DID modalias, if known.
        model_id: Option<String>,
        /// Whether the watcher selected this address from an explicit pin.
        /// This is only authority for an explicitly armed `connect-once`.
        pinned: bool,
    },
    /// BlueZ `Connected` for the classic link.
    Connected(bool),
    /// An audio profile endpoint or media transport under the device appeared,
    /// disappeared or changed `State`. The handoff hold uses it to tell a
    /// player resuming on a new sink from a user pressing play.
    AudioTransport,
    /// Whether an A2DP `MediaTransport1` under the device is `pending` or
    /// `active`, with every known transport as `path uuid state`. Sent when
    /// the watch starts and whenever it changes; the handoff uses it to tell
    /// a take-over whose audio never started.
    A2dpTransport {
        /// An A2DP transport is pending or active.
        active: bool,
        /// Each transport under the device, for the log.
        transports: String,
    },
    /// A non-A2DP (hands-free) `MediaTransport1` became pending or active:
    /// the audio is on the wrong profile. Logged only; no profile is changed.
    HandsFreeTransport {
        /// The hands-free transports, as `path uuid state`.
        transports: String,
    },
    /// Whether the local adapter's Device ID (`Modalias`) names Apple, which
    /// Apple hosts require before they take part in ownership handoff.
    /// `None` when the property is missing or unreadable.
    AdapterAppleId(Option<bool>),
    /// BlueZ `Device1.Disconnected(ss)` for the device, with the adapter and
    /// device state read when it arrived.
    Disconnected {
        /// Reason name, for example `org.bluez.Reason.Remote`.
        reason: String,
        /// Free-text message BlueZ sends with it.
        message: String,
        /// `None` when the state could not be read.
        facts: Option<rejoin::LinkFacts>,
    },
    /// The adapter or bluetoothd went away; treat as disconnected.
    ///
    /// Only sent on evidence about the adapter itself: its object removed,
    /// `Adapter1.Powered` false, or the adapter unreadable. A watch that
    /// merely ended is [`LinkEvent::WatchEnded`].
    AdapterGone,
    /// The device property watch ended while the adapter was still there.
    ///
    /// BlueZ emits `InterfacesRemoved` for the device object whenever one of
    /// its profile interfaces (`Battery1`, `MediaControl1`, ...) goes away at
    /// disconnect, and `bluer` cancels the whole per-object subscription on
    /// that signal, so the stream ends with `Device1` and the adapter still
    /// present. This says nothing about the link, the adapter, or a rejoin
    /// already scheduled; the watcher rebuilds itself.
    WatchEnded,
}

/// Does this device look like an AirPods-family accessory?
///
/// Either the DID modalias says Apple (`bluetooth:v004C...`) or the name
/// contains "AirPods".
async fn looks_like_airpods(dev: &Device) -> bool {
    if let Ok(Some(m)) = dev.modalias().await {
        if m.source == "bluetooth" && m.vendor == models::APPLE_VENDOR_ID {
            return true;
        }
    }
    matches!(dev.name().await, Ok(Some(n)) if n.to_ascii_lowercase().contains("airpods"))
}

async fn model_id_of(dev: &Device) -> Option<String> {
    let m = dev.modalias().await.ok().flatten()?;
    (m.vendor == models::APPLE_VENDOR_ID).then(|| models::model_id(m.product))
}

/// Find the device to watch: the pinned address if configured, otherwise the
/// first paired device that looks like AirPods.
async fn pick_device(adapter: &Adapter, pinned: Option<Address>) -> bluer::Result<Option<Device>> {
    if let Some(addr) = pinned {
        return adapter.device(addr).map(Some);
    }
    for addr in adapter.device_addresses().await? {
        let dev = adapter.device(addr)?;
        if !dev.is_paired().await.unwrap_or(false) {
            continue;
        }
        if looks_like_airpods(&dev).await {
            return Ok(Some(dev));
        }
    }
    Ok(None)
}

/// Run the BlueZ watcher forever, rebuilding the session on any failure.
pub async fn run(tx: mpsc::Sender<LinkEvent>, pinned: Option<Address>) {
    loop {
        if let Err(e) = once(&tx, pinned).await {
            warn!(error = %e, "BlueZ watcher failed; rebuilding");
            let _ = tx.send(LinkEvent::AdapterGone).await;
        }
        tokio::time::sleep(REBUILD_DELAY).await;
    }
}

async fn once(tx: &mpsc::Sender<LinkEvent>, pinned: Option<Address>) -> bluer::Result<()> {
    let session = Session::new().await?;
    let adapter = session.default_adapter().await?;
    let adapter_addr = adapter.address().await?;

    let Some(dev) = pick_device(&adapter, pinned).await? else {
        debug!("no paired AirPods-like device yet");
        // Nothing to watch: tell the supervisor so it tears any link down
        // instead of believing a stale Connected=true.
        let _ = tx.send(LinkEvent::AdapterGone).await;
        tokio::time::sleep(RESCAN_DELAY).await;
        return Ok(());
    };
    let address = dev.address();
    let name = dev.name().await.ok().flatten();
    let model_id = model_id_of(&dev).await;
    info!(%address, name = ?name, model_id = ?model_id, "watching device");
    let _ = tx
        .send(LinkEvent::Identity {
            adapter: adapter_addr,
            address,
            name,
            model_id,
            pinned: pinned.is_some(),
        })
        .await;
    let apple_host_id = match adapter.modalias().await {
        Ok(Some(m)) => Some(m.source == "bluetooth" && m.vendor == models::APPLE_VENDOR_ID),
        Ok(None) | Err(_) => None,
    };
    let _ = tx.send(LinkEvent::AdapterAppleId(apple_host_id)).await;

    let device_path = format!(
        "/org/bluez/{}/dev_{}",
        adapter.name(),
        address.to_string().replace(':', "_")
    );
    let (disconnect_tx, mut disconnects) = mpsc::channel::<(String, String)>(4);
    let disconnect_path = device_path.clone();
    let _disconnects = AbortOnDrop(tokio::spawn(async move {
        if let Err(e) = watch_disconnected(&disconnect_tx, &disconnect_path).await {
            warn!(error = %e, "BlueZ Disconnected signal watch unavailable; no rejoin after eviction");
        }
    }));
    let audio_tx = tx.clone();
    let _audio = AbortOnDrop(tokio::spawn(async move {
        if let Err(e) = watch_audio_objects(&audio_tx, &device_path).await {
            debug!(error = %e, "BlueZ audio transport watch unavailable");
        }
    }));

    let mut events = Box::pin(dev.events().await?);
    let mut connected = dev.is_connected().await?;
    let _ = tx.send(LinkEvent::Connected(connected)).await;

    let mut reconcile = tokio::time::interval(RECONCILE);
    reconcile.tick().await; // fires immediately; skip

    loop {
        tokio::select! {
            ev = events.next() => match ev {
                Some(DeviceEvent::PropertyChanged(prop)) => match prop {
                    DeviceProperty::Connected(v) => {
                        connected = v;
                        debug!(connected = v, "BlueZ Connected changed");
                        let _ = tx.send(LinkEvent::Connected(v)).await;
                    }
                    // BlueZ re-reads the remote `Name` only when a link
                    // opens, and a rename usually surfaces as `Alias` first.
                    // Either one is the same fact about the same device. The
                    // daemon never writes `Alias` itself.
                    DeviceProperty::Name(n) | DeviceProperty::Alias(n) => {
                        let _ = tx.send(LinkEvent::Identity {
                            adapter: adapter_addr, address, name: Some(n), model_id: None,
                            pinned: pinned.is_some(),
                        }).await;
                    }
                    DeviceProperty::Modalias(m) => {
                        let model_id = (m.vendor == models::APPLE_VENDOR_ID)
                            .then(|| models::model_id(m.product));
                        let _ = tx.send(LinkEvent::Identity {
                            adapter: adapter_addr, address, name: None, model_id,
                            pinned: pinned.is_some(),
                        }).await;
                    }
                    other => debug!(?other, "ignored device property"),
                },
                None => {
                    // The subscription ended, which is not evidence about the
                    // adapter: ask the adapter itself before saying it is gone.
                    match adapter.is_powered().await {
                        Ok(true) => {
                            warn!("device event stream ended; rebuilding the watch");
                            let _ = tx.send(LinkEvent::WatchEnded).await;
                        }
                        Ok(false) => {
                            warn!("device event stream ended; adapter is powered off");
                            let _ = tx.send(LinkEvent::AdapterGone).await;
                        }
                        Err(e) => {
                            warn!(error = %e, "device event stream ended; adapter unreadable");
                            let _ = tx.send(LinkEvent::AdapterGone).await;
                        }
                    }
                    return Ok(());
                }
            },
            Some((reason, message)) = disconnects.recv() => {
                // Read before anything else changes, as close to the signal
                // as possible.
                let facts = link_facts(&adapter, &dev).await;
                info!(reason, message, ?facts, "BlueZ Disconnected");
                let _ = tx.send(LinkEvent::Disconnected { reason, message, facts }).await;
            }
            _ = reconcile.tick() => {
                // Belt and braces: a missed signal must not strand the daemon.
                match dev.is_connected().await {
                    Ok(actual) if actual != connected => {
                        warn!(believed = connected, actual, "reconciling Connected");
                        connected = actual;
                        let _ = tx.send(LinkEvent::Connected(actual)).await;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!(error = %e, "device vanished");
                        return Ok(());
                    }
                }
            }
        }
    }
}

/// Re-read BlueZ immediately before an AAP dial.
///
/// L2CAP `connect()` can make the kernel establish an ACL.  The watcher event
/// is therefore only a hint: callers must require a fresh local `Connected`
/// read for the same adapter and device before opening the AAP PSM.  This
/// helper never calls `Device1.Connect`.
pub async fn locally_connected(
    adapter_address: Address,
    device_address: Address,
) -> bluer::Result<bool> {
    let session = Session::new().await?;
    for name in session.adapter_names().await? {
        let adapter = session.adapter(&name)?;
        if adapter.address().await? != adapter_address {
            continue;
        }
        let device = adapter.device(device_address)?;
        return Ok(device.is_paired().await? && device.is_connected().await?);
    }
    Ok(false)
}

/// Resolve the paired pinned device without a connection side effect. Return
/// its handle so the supervisor can recheck command authority after these
/// asynchronous lookups, immediately before issuing `Device1.Connect`.
pub async fn paired_device(
    adapter_address: Address,
    device_address: Address,
) -> bluer::Result<Option<Device>> {
    let session = Session::new().await?;
    for name in session.adapter_names().await? {
        let adapter = session.adapter(&name)?;
        if adapter.address().await? != adapter_address {
            continue;
        }
        let device = adapter.device(device_address)?;
        if !device.is_paired().await? {
            return Ok(None);
        }
        return Ok(Some(device));
    }
    Ok(None)
}

/// Adapter `Powered` and device `Paired`, `Blocked`, or `None` if any of them
/// cannot be read. `Trusted` is not needed for an outgoing connect.
async fn link_facts(adapter: &Adapter, dev: &Device) -> Option<rejoin::LinkFacts> {
    Some(rejoin::LinkFacts {
        powered: adapter.is_powered().await.ok()?,
        paired: dev.is_paired().await.ok()?,
        blocked: dev.is_blocked().await.ok()?,
    })
}

/// Rejoin after an Apple host evicted this host: re-check that the adapter is
/// powered, the device paired, not blocked and still disconnected,
/// and that `still_wanted` holds, then call `Device1.Connect` once. Never
/// calls `Disconnect`, not even to clear a stuck `br-connection-busy`.
pub async fn rejoin_connect(
    adapter_address: Address,
    device_address: Address,
    still_wanted: impl Fn() -> bool,
) -> rejoin::Outcome {
    match rejoin_connect_checked(adapter_address, device_address, still_wanted).await {
        Ok(outcome) => outcome,
        Err(e) if e.kind == ErrorKind::AlreadyConnected => {
            rejoin::Outcome::NotReady("connected_by_other_means")
        }
        Err(e) if profile_busy(&e) => rejoin::Outcome::Busy,
        Err(e) => rejoin::Outcome::Failed(e.to_string()),
    }
}

async fn rejoin_connect_checked(
    adapter_address: Address,
    device_address: Address,
    still_wanted: impl Fn() -> bool,
) -> bluer::Result<rejoin::Outcome> {
    use rejoin::Outcome::{Connected, NotReady};
    let session = Session::new().await?;
    for name in session.adapter_names().await? {
        let adapter = session.adapter(&name)?;
        if adapter.address().await? != adapter_address {
            continue;
        }
        if !adapter.is_powered().await? {
            return Ok(NotReady("adapter_off"));
        }
        let device = adapter.device(device_address)?;
        if !device.is_paired().await? {
            return Ok(NotReady("not_paired"));
        }
        if device.is_blocked().await? {
            return Ok(NotReady("blocked"));
        }
        if device.is_connected().await? {
            return Ok(NotReady("connected_by_other_means"));
        }
        if !still_wanted() {
            return Ok(NotReady("superseded"));
        }
        device.connect().await?;
        return Ok(Connected);
    }
    Ok(NotReady("adapter_gone"))
}

/// A2DP sink, the accessory side of stereo audio (remote profile UUID).
pub const A2DP_SINK_UUID: bluer::Uuid =
    bluer::Uuid::from_u128(0x0000110b_0000_1000_8000_00805f9b34fb);

async fn device_on(
    adapter_address: Address,
    device_address: Address,
) -> bluer::Result<Option<Device>> {
    let session = Session::new().await?;
    for name in session.adapter_names().await? {
        let adapter = session.adapter(&name)?;
        if adapter.address().await? == adapter_address {
            return adapter.device(device_address).map(Some);
        }
    }
    Ok(None)
}

/// Take audio back: connect A2DP sink on an already connected device. A
/// profile that is already connected is success: yielding keeps profiles up.
///
/// Yield never releases profiles: disconnecting A2DP/HFP on yield does not
/// work, because the audio server can reconnect A2DP by itself about ten
/// seconds later and move a still-playing browser stream onto it, stealing
/// the AirPods back from the Mac. Cycling the profile after a take-over does
/// not work either: it tears the whole link down and leaves the audio on the
/// hands-free profile. Nothing here ever disconnects a profile; the PipeWire
/// card profile is switched instead (see `audio_route`).
pub async fn claim_audio(adapter_address: Address, device_address: Address) -> bluer::Result<()> {
    let Some(device) = device_on(adapter_address, device_address).await? else {
        return Ok(());
    };
    match device.connect_profile(&A2DP_SINK_UUID).await {
        Err(e) if e.kind == ErrorKind::AlreadyConnected => Ok(()),
        other => other,
    }
}

/// A profile connect refused because the link is busy (`br-connection-busy`)
/// or another connect is in progress; worth retrying shortly.
pub fn profile_busy(error: &bluer::Error) -> bool {
    error.kind == ErrorKind::InProgress || error.message.contains("busy")
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// One `MediaTransport1` under the device.
#[derive(Debug, Clone)]
struct Transport {
    uuid: String,
    a2dp: bool,
    active: bool,
}

/// Media transports under the device, by object path. Hands-free transports
/// are kept as well as A2DP ones, so audio landing on the wrong profile is
/// visible in the log.
#[derive(Debug, Default)]
struct AudioTransports {
    by_path: HashMap<String, Transport>,
}

impl AudioTransports {
    /// A `MediaTransport1` appeared with these properties.
    fn add(&mut self, path: &str, props: &PropMap) {
        let Some(uuid) = prop_cast::<String>(props, "UUID") else {
            return;
        };
        let uuid = uuid.to_ascii_lowercase();
        let a2dp = a2dp_uuid(&uuid);
        let active = prop_cast::<String>(props, "State").is_some_and(|s| streaming_state(s));
        self.by_path
            .insert(path.to_owned(), Transport { uuid, a2dp, active });
    }

    /// `PropertiesChanged` on a `MediaTransport1`.
    fn changed(&mut self, path: &str, changed: &PropMap) {
        if let (Some(transport), Some(state)) = (
            self.by_path.get_mut(path),
            prop_cast::<String>(changed, "State"),
        ) {
            transport.active = streaming_state(state);
        }
    }

    fn remove(&mut self, path: &str) {
        self.by_path.remove(path);
    }

    /// An A2DP transport is pending or active.
    fn active(&self) -> bool {
        self.by_path.values().any(|t| t.a2dp && t.active)
    }

    /// A non-A2DP transport is pending or active: the audio is on the
    /// hands-free profile.
    fn hands_free(&self) -> bool {
        self.by_path.values().any(|t| !t.a2dp && t.active)
    }

    /// Every known transport as `path uuid state`, for the log.
    fn describe(&self) -> String {
        self.list(|_| true)
    }

    /// The transports that are pending or active and not A2DP.
    fn hands_free_detail(&self) -> String {
        self.list(|t| !t.a2dp && t.active)
    }

    fn list(&self, keep: impl Fn(&Transport) -> bool) -> String {
        let mut lines: Vec<String> = self
            .by_path
            .iter()
            .filter(|(_, t)| keep(t))
            .map(|(path, t)| {
                let state = if t.active { "streaming" } else { "idle" };
                format!("{path} {} {state}", t.uuid)
            })
            .collect();
        lines.sort();
        if lines.is_empty() {
            "none".to_owned()
        } else {
            lines.join(", ")
        }
    }
}

/// A2DP source (the local endpoint BlueZ names for headphones) or sink.
fn a2dp_uuid(uuid: &str) -> bool {
    let uuid = uuid.to_ascii_lowercase();
    uuid == "0000110a-0000-1000-8000-00805f9b34fb" || uuid == A2DP_SINK_UUID.to_string()
}

/// `MediaTransport1.State` values that mean audio flows or is about to.
fn streaming_state(state: &str) -> bool {
    matches!(state, "pending" | "active")
}

/// Report profile endpoints and media transports appearing, disappearing or
/// changing `State` under `device_path`, and whether the A2DP transport is
/// streaming, until the system bus goes away.
async fn watch_audio_objects(
    tx: &mpsc::Sender<LinkEvent>,
    device_path: &str,
) -> Result<(), dbus::Error> {
    let (resource, conn) = dbus_tokio::connection::new_system_sync()?;
    let (lost_tx, mut lost_rx) = tokio::sync::oneshot::channel::<()>();
    let _io = AbortOnDrop(tokio::spawn(async move {
        let err = resource.await;
        debug!(error = %err, "BlueZ audio watch bus connection lost");
        let _ = lost_tx.send(());
    }));
    let subscribe = |rule: MatchRule<'static>, conn: &Arc<SyncConnection>| {
        let conn = Arc::clone(conn);
        async move { conn.add_match(rule).await.map(MsgMatch::msg_stream) }
    };
    let manager = "org.freedesktop.DBus.ObjectManager";
    let (_added_match, mut added) = subscribe(
        MatchRule::new_signal(manager, "InterfacesAdded").with_sender("org.bluez"),
        &conn,
    )
    .await?;
    let (_removed_match, mut removed) = subscribe(
        MatchRule::new_signal(manager, "InterfacesRemoved").with_sender("org.bluez"),
        &conn,
    )
    .await?;
    let (_props_match, mut props) = subscribe(
        MatchRule::new_signal("org.freedesktop.DBus.Properties", "PropertiesChanged")
            .with_sender("org.bluez")
            .with_namespaced_path(device_path.to_owned()),
        &conn,
    )
    .await?;
    let prefix = format!("{device_path}/");

    // Subscribed first, so no change between this read and the signals is lost.
    let proxy = Proxy::new("org.bluez", "/", MANAGED_OBJECTS_TIMEOUT, Arc::clone(&conn));
    let (objects,): (HashMap<dbus::Path<'static>, HashMap<String, PropMap>>,) =
        proxy.method_call(manager, "GetManagedObjects", ()).await?;
    let mut transports = AudioTransports::default();
    for (path, interfaces) in &objects {
        if let Some(p) = interfaces
            .get(TRANSPORT_IFACE)
            .filter(|_| path.starts_with(&prefix))
        {
            transports.add(path, p);
        }
    }
    let mut reported = transports.active();
    let mut hands_free = transports.hands_free();
    if tx
        .send(LinkEvent::A2dpTransport {
            active: reported,
            transports: transports.describe(),
        })
        .await
        .is_err()
    {
        return Ok(());
    }
    if hands_free
        && tx
            .send(LinkEvent::HandsFreeTransport {
                transports: transports.hands_free_detail(),
            })
            .await
            .is_err()
    {
        return Ok(());
    }

    loop {
        let relevant = tokio::select! {
            _ = &mut lost_rx => return Ok(()),
            m = added.next() => match m {
                Some(m) => {
                    if let (Some(path), Some(interfaces)) =
                        m.get2::<dbus::Path, HashMap<String, PropMap>>()
                    {
                        if let Some(p) = interfaces
                            .get(TRANSPORT_IFACE)
                            .filter(|_| path.starts_with(&prefix))
                        {
                            transports.add(&path, p);
                        }
                    }
                    object_under(&m, &prefix)
                }
                None => return Ok(()),
            },
            m = removed.next() => match m {
                Some(m) => {
                    if let Some(path) = m.get1::<dbus::Path>() {
                        transports.remove(&path);
                    }
                    object_under(&m, &prefix)
                }
                None => return Ok(()),
            },
            m = props.next() => match m {
                Some(m) => {
                    if let (Some(path), (Some(iface), Some(changed))) =
                        (m.path(), m.get2::<&str, PropMap>())
                    {
                        if iface == TRANSPORT_IFACE {
                            transports.changed(&path, &changed);
                        }
                    }
                    transport_state_changed(&m, &prefix)
                }
                None => return Ok(()),
            },
        };
        if relevant {
            debug!("BlueZ audio profile or transport changed");
            if tx.send(LinkEvent::AudioTransport).await.is_err() {
                return Ok(());
            }
        }
        let active = transports.active();
        if active != reported {
            reported = active;
            if tx
                .send(LinkEvent::A2dpTransport {
                    active,
                    transports: transports.describe(),
                })
                .await
                .is_err()
            {
                return Ok(());
            }
        }
        let now_hands_free = transports.hands_free();
        if now_hands_free
            && !hands_free
            && tx
                .send(LinkEvent::HandsFreeTransport {
                    transports: transports.hands_free_detail(),
                })
                .await
                .is_err()
        {
            return Ok(());
        }
        hands_free = now_hands_free;
    }
}

/// Forward `Device1.Disconnected(reason, message)` for `device_path` until the
/// system bus goes away.
async fn watch_disconnected(
    tx: &mpsc::Sender<(String, String)>,
    device_path: &str,
) -> Result<(), dbus::Error> {
    let (resource, conn) = dbus_tokio::connection::new_system_sync()?;
    let (lost_tx, mut lost_rx) = tokio::sync::oneshot::channel::<()>();
    let _io = AbortOnDrop(tokio::spawn(async move {
        let err = resource.await;
        debug!(error = %err, "BlueZ Disconnected watch bus connection lost");
        let _ = lost_tx.send(());
    }));
    let rule = MatchRule::new_signal(DEVICE_IFACE, "Disconnected")
        .with_sender("org.bluez")
        .with_path(device_path.to_owned());
    let (_match, mut signals) = conn.add_match(rule).await?.msg_stream();
    loop {
        tokio::select! {
            _ = &mut lost_rx => return Ok(()),
            m = signals.next() => match m {
                Some(m) => {
                    let Some(args) = disconnected_args(&m) else { continue };
                    if tx.send(args).await.is_err() {
                        return Ok(());
                    }
                }
                None => return Ok(()),
            },
        }
    }
}

/// `(reason, message)`; the message is optional so a one-argument form still
/// reports its reason.
fn disconnected_args(m: &Message) -> Option<(String, String)> {
    let (reason, message) = m.get2::<String, String>();
    Some((reason?, message.unwrap_or_default()))
}

fn object_under(m: &Message, prefix: &str) -> bool {
    m.get1::<dbus::Path>()
        .is_some_and(|p| p.starts_with(prefix))
}

fn transport_state_changed(m: &Message, prefix: &str) -> bool {
    let under = m.path().is_some_and(|p| p.starts_with(prefix));
    under
        && matches!(
            m.get2::<&str, PropMap>(),
            (Some("org.bluez.MediaTransport1"), Some(changed)) if changed.contains_key("State")
        )
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    fn err(kind: ErrorKind, message: &str) -> bluer::Error {
        bluer::Error {
            kind,
            message: message.to_owned(),
        }
    }

    fn props(entries: &[(&str, &str)]) -> PropMap {
        entries
            .iter()
            .map(|(k, v)| {
                let value: Box<dyn dbus::arg::RefArg> = Box::new((*v).to_owned());
                ((*k).to_owned(), dbus::arg::Variant(value))
            })
            .collect()
    }

    #[test]
    fn only_a_pending_or_active_a2dp_transport_counts_as_streaming() {
        // As read live: sep2/fd0 is the local A2DP source endpoint's UUID.
        let fd0 = "/org/bluez/hci0/dev_BC_80_4E_01_02_03/sep2/fd0";
        let mut t = AudioTransports::default();
        t.add(
            fd0,
            &props(&[
                ("UUID", "0000110a-0000-1000-8000-00805f9b34fb"),
                ("State", "idle"),
            ]),
        );
        // An HFP transport, active, is not A2DP.
        t.add(
            "/org/bluez/hci0/dev_BC_80_4E_01_02_03/fd1",
            &props(&[
                ("UUID", "0000111e-0000-1000-8000-00805f9b34fb"),
                ("State", "active"),
            ]),
        );
        assert!(!t.active());
        // The hands-free transport is visible, with its UUID and path.
        assert!(t.hands_free());
        assert_eq!(
            t.hands_free_detail(),
            concat!(
                "/org/bluez/hci0/dev_BC_80_4E_01_02_03/fd1 ",
                "0000111e-0000-1000-8000-00805f9b34fb streaming"
            )
        );
        assert!(t
            .describe()
            .contains("sep2/fd0 0000110a-0000-1000-8000-00805f9b34fb idle"));
        t.changed(fd0, &props(&[("State", "pending")]));
        assert!(t.active());
        t.changed(fd0, &props(&[("Volume", "x")]));
        assert!(t.active());
        t.changed(fd0, &props(&[("State", "active")]));
        assert!(t.active());
        t.changed(fd0, &props(&[("State", "idle")]));
        assert!(!t.active());
        t.changed(fd0, &props(&[("State", "active")]));
        t.remove(fd0);
        assert!(!t.active());
        // A state change for a transport never added is ignored.
        t.changed(fd0, &props(&[("State", "active")]));
        assert!(!t.active());
    }

    #[test]
    fn disconnected_signal_arguments_are_read_with_an_optional_message() {
        let path = "/org/bluez/hci0/dev_BC_80_4E_01_02_03";
        let m = Message::new_signal(path, DEVICE_IFACE, "Disconnected")
            .unwrap()
            .append2(
                "org.bluez.Reason.Remote",
                "Connection terminated by remote host",
            );
        assert_eq!(
            disconnected_args(&m),
            Some((
                "org.bluez.Reason.Remote".to_owned(),
                "Connection terminated by remote host".to_owned()
            ))
        );
        let m = Message::new_signal(path, DEVICE_IFACE, "Disconnected")
            .unwrap()
            .append1("org.bluez.Reason.Local");
        assert_eq!(
            disconnected_args(&m),
            Some(("org.bluez.Reason.Local".to_owned(), String::new()))
        );
        let m = Message::new_signal(path, DEVICE_IFACE, "Disconnected").unwrap();
        assert_eq!(disconnected_args(&m), None);
    }

    #[test]
    fn busy_and_in_progress_connects_are_retryable() {
        // As logged live: "Bluetooth operation in progress: br-connection-busy".
        assert!(profile_busy(&err(
            ErrorKind::InProgress,
            "br-connection-busy"
        )));
        assert!(profile_busy(&err(ErrorKind::Failed, "br-connection-busy")));
        assert!(!profile_busy(&err(
            ErrorKind::Failed,
            "br-connection-refused"
        )));
        assert!(!profile_busy(&err(ErrorKind::NotAvailable, "")));
    }
}
