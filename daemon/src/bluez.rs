//! BlueZ side: pick the device, then watch its `Connected` property.
//!
//! This task NEVER initiates a connection. It only reads properties and
//! subscribes to `PropertiesChanged`; the accessory is connected by the user
//! or by BlueZ's own auto-connect.

use std::time::Duration;

use bluer::{Adapter, Address, Device, DeviceEvent, DeviceProperty, Session};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::models;

/// How long to wait before rebuilding the D-Bus chain after an error.
const REBUILD_DELAY: Duration = Duration::from_secs(2);
/// How long to wait when no matching device is paired yet.
const RESCAN_DELAY: Duration = Duration::from_secs(10);
/// Reconciliation tick: re-read `Connected` in case an event was missed.
const RECONCILE: Duration = Duration::from_secs(30);

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
    },
    /// BlueZ `Connected` for the classic link.
    Connected(bool),
    /// The adapter or bluetoothd went away; treat as disconnected.
    AdapterGone,
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
        })
        .await;

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
                    DeviceProperty::Name(n) => {
                        let _ = tx.send(LinkEvent::Identity {
                            adapter: adapter_addr, address, name: Some(n), model_id: None,
                        }).await;
                    }
                    DeviceProperty::Modalias(m) => {
                        let model_id = (m.vendor == models::APPLE_VENDOR_ID)
                            .then(|| models::model_id(m.product));
                        let _ = tx.send(LinkEvent::Identity {
                            adapter: adapter_addr, address, name: None, model_id,
                        }).await;
                    }
                    other => debug!(?other, "ignored device property"),
                },
                None => {
                    warn!("device event stream ended");
                    let _ = tx.send(LinkEvent::AdapterGone).await;
                    return Ok(());
                }
            },
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
