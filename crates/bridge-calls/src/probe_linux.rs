// SPDX-License-Identifier: GPL-3.0-or-later
//! Linux implementation of the read-only probe.

use std::collections::HashMap;
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::blocking::fdo::DBusProxy;
use zbus::names::BusName;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use crate::declared::{IFACE_OBJECT_MANAGER, MANAGER_PATH, SERVICE};
use crate::model::{CallObservation, PhoneSnapshot, ServiceStatus};
use crate::pipewire::{BusTarget, Client, parse_introspection};
use crate::probe::*;

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const HFP_AG_UUID: &str = "0000111f-0000-1000-8000-00805f9b34fb";

type Props = HashMap<String, OwnedValue>;
type Managed = HashMap<OwnedObjectPath, HashMap<String, Props>>;

pub(crate) fn run(options: &ProbeOptions) -> ProbeReport {
    let mut buses = Vec::new();
    let mut bluez = None;
    for (label, choice, is_system) in [
        ("session", &options.session, false),
        ("system", &options.system, true),
    ] {
        let (bus_label, target) = match choice {
            BusChoice::Skip => continue,
            BusChoice::Default if is_system => (label.to_string(), BusTarget::System),
            BusChoice::Default => (label.to_string(), BusTarget::Session),
            BusChoice::Address(a) => (format!("address:{label}"), BusTarget::Address(a.clone())),
        };
        match target.connect(PROBE_TIMEOUT) {
            Ok(conn) => {
                let (bus, bz) = probe_bus(&bus_label, conn, is_system, options.show_private);
                buses.push(bus);
                if is_system {
                    bluez = bz;
                }
            }
            Err(e) => buses.push(BusProbe {
                bus: bus_label,
                connected: false,
                error: Some(e.to_string()),
                telephony: None,
                ofono_name_owned: None,
            }),
        }
    }
    let (summary, statements) = summarize(true, &buses, bluez.as_ref());
    let capability_evidence = buses
        .iter()
        .filter_map(|b| b.telephony.as_ref())
        .flat_map(|t| &t.phones)
        .map(|p| p.capabilities.clone())
        .next()
        .unwrap_or_else(no_phone_evidence);
    ProbeReport {
        schema: REPORT_SCHEMA,
        tool: "geekbridge-call-probe",
        read_only: true,
        platform: "linux".into(),
        backend: "pipewire_telephony_dbus",
        redacted: !options.show_private,
        buses,
        bluez,
        summary,
        capability_evidence,
        hardware_verified: Vec::new(),
        static_api: static_api_reference(),
        statements,
    }
}

fn has_owner(conn: &Connection, name: &str) -> Result<bool, String> {
    let dbus = DBusProxy::new(conn).map_err(|e| e.to_string())?;
    let n = BusName::try_from(name).map_err(|e| e.to_string())?;
    dbus.name_has_owner(n).map_err(|e| e.to_string())
}

fn process_name(conn: &Connection, name: &str) -> Option<String> {
    let dbus = DBusProxy::new(conn).ok()?;
    let pid = dbus
        .get_connection_unix_process_id(BusName::try_from(name).ok()?)
        .ok()?;
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(format!("{} (pid {pid})", comm.trim()))
}

fn probe_bus(
    label: &str,
    conn: Connection,
    is_system: bool,
    show_private: bool,
) -> (BusProbe, Option<BluezProbe>) {
    let ofono = has_owner(&conn, "org.ofono").ok();
    let bluez = is_system.then(|| probe_bluez(&conn, show_private));
    let client = Client::new(conn.clone());
    let telephony = match client.owner() {
        Err(e) => TelephonyProbe {
            service: SERVICE,
            owner: None,
            owner_process: None,
            manager_interfaces: vec![],
            phones: vec![],
            warnings: vec![format!("owner lookup failed: {e}")],
        },
        Ok(None) => TelephonyProbe {
            service: SERVICE,
            owner: None,
            owner_process: None,
            manager_interfaces: vec![],
            phones: vec![],
            warnings: vec![],
        },
        Ok(Some(owner)) => {
            let manager_interfaces = conn
                .call_method(
                    Some(owner.as_str()),
                    MANAGER_PATH,
                    Some("org.freedesktop.DBus.Introspectable"),
                    "Introspect",
                    &(),
                )
                .ok()
                .and_then(|m| m.body().deserialize::<String>().ok())
                .and_then(|xml| parse_introspection(&xml).ok())
                .map(|m| m.into_keys().collect())
                .unwrap_or_default();
            let mut warnings = Vec::new();
            let phones = match client.snapshot_filtered(None) {
                Ok(s) => {
                    warnings.extend(s.warnings.iter().cloned());
                    match s.service {
                        ServiceStatus::Present { .. } => s
                            .phones
                            .iter()
                            .map(|p| phone_probe(p, show_private))
                            .collect(),
                        ServiceStatus::Absent => vec![],
                    }
                }
                Err(e) => {
                    warnings.push(format!("snapshot failed: {e}"));
                    vec![]
                }
            };
            TelephonyProbe {
                service: SERVICE,
                owner_process: process_name(&conn, SERVICE),
                owner: Some(owner),
                manager_interfaces,
                phones,
                warnings,
            }
        }
    };
    (
        BusProbe {
            bus: label.to_string(),
            connected: true,
            error: None,
            telephony: Some(telephony),
            ofono_name_owned: ofono,
        },
        bluez,
    )
}

fn phone_probe(p: &PhoneSnapshot, show_private: bool) -> PhoneProbe {
    let addr = p.device.address.expose();
    PhoneProbe {
        path: p.device.path.clone(),
        address: if show_private {
            addr.to_string()
        } else {
            mask_address(addr)
        },
        transport: p.transport.as_ref().map(|t| TransportProbe {
            state: t.state,
            raw_state: t.raw_state.clone(),
            codec_id: t.codec_id,
            codec: t.codec_evidence,
        }),
        calls: p
            .calls
            .iter()
            .map(|c| CallProbe {
                path: c.path.clone(),
                state: match &c.observation {
                    CallObservation::Unrecognized(s) => format!("unrecognized:{s}"),
                    _ => c.raw_state.clone(),
                },
                direction: format!("{:?}", c.direction()).to_lowercase(),
                number: c.fingerprint.line_identification.as_ref().map(|n| {
                    if show_private {
                        n.expose().to_string()
                    } else {
                        mask_number(n.expose())
                    }
                }),
                name: c.fingerprint.name.as_ref().map(|n| {
                    if show_private {
                        n.expose().to_string()
                    } else {
                        "<redacted>".into()
                    }
                }),
            })
            .collect(),
        declared_api: p.declared_api.clone(),
        capabilities: p.evidence.clone(),
    }
}

fn probe_bluez(conn: &Connection, show_private: bool) -> BluezProbe {
    let mut out = BluezProbe {
        service_present: false,
        owner_process: None,
        adapters: vec![],
        devices: vec![],
        telephony1_objects: 0,
        call1_objects: 0,
        error: None,
    };
    match has_owner(conn, "org.bluez") {
        Ok(true) => out.service_present = true,
        Ok(false) => return out,
        Err(e) => {
            out.error = Some(e);
            return out;
        }
    }
    out.owner_process = process_name(conn, "org.bluez");
    let managed: Result<Managed, String> = conn
        .call_method(
            Some("org.bluez"),
            "/",
            Some(IFACE_OBJECT_MANAGER),
            "GetManagedObjects",
            &(),
        )
        .map_err(|e| e.to_string())
        .and_then(|m| m.body().deserialize::<Managed>().map_err(|e| e.to_string()));
    let managed = match managed {
        Ok(m) => m,
        Err(e) => {
            out.error = Some(e);
            return out;
        }
    };
    let mask = |a: String| if show_private { a } else { mask_address(&a) };
    let s = |p: &Props, k: &str| {
        p.get(k)
            .and_then(|v| v.downcast_ref::<&str>().ok().map(str::to_owned))
    };
    let b = |p: &Props, k: &str| p.get(k).and_then(|v| v.downcast_ref::<bool>().ok());
    let mut paths: Vec<_> = managed.iter().collect();
    paths.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    for (path, ifaces) in paths {
        if let Some(a) = ifaces.get("org.bluez.Adapter1") {
            out.adapters.push(AdapterProbe {
                path: path.to_string(),
                address: s(a, "Address").map(mask),
                powered: b(a, "Powered"),
            });
        }
        if let Some(d) = ifaces.get("org.bluez.Device1") {
            let uuids: Vec<String> = d
                .get("UUIDs")
                .and_then(|v| v.try_clone().ok())
                .and_then(|v| Vec::<String>::try_from(v).ok())
                .unwrap_or_default();
            out.devices.push(BluezDeviceProbe {
                path: path.to_string(),
                address: s(d, "Address").map(mask),
                connected: b(d, "Connected"),
                paired: b(d, "Paired"),
                hfp_audio_gateway: uuids.iter().any(|u| u.eq_ignore_ascii_case(HFP_AG_UUID)),
            });
        }
        out.telephony1_objects += usize::from(ifaces.contains_key("org.bluez.Telephony1"));
        out.call1_objects += usize::from(ifaces.contains_key("org.bluez.Call1"));
    }
    out
}
