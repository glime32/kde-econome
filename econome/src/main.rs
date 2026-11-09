// econome: three small battery savings in one sleeping process.
//   - Bluetooth is rfkill-blocked after 10 min with nothing connected
//   - the built-in screen goes to 60 Hz on battery, back to its fastest rate on AC
//   - plugged in, full, and not in Conservation for 20 min: back to Conservation
//
// It waits on the system bus (UPower, BlueZ). The only timers: every 5 min while
// on AC (the charge mode has no signal), and the Bluetooth deadline when armed.
//
// Command line: econome            daemon
//               econome ecran batterie|secteur
//               econome etat

use dbus::arg::{prop_cast, PropMap, RefArg, Variant};
use dbus::blocking::stdintf::org_freedesktop_dbus::ObjectManager;
use dbus::blocking::Connection;
use dbus::channel::MatchingReceiver;
use dbus::message::MatchRule;
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const BAT: &str = "/sys/class/power_supply/BAT0";
const PROPS: &str = "org.freedesktop.DBus.Properties";
const MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const T: Duration = Duration::from_secs(5);
const AC_POLL: Duration = Duration::from_secs(5 * 60);
const BT_SETTLE: Duration = Duration::from_secs(2);

fn delay(var: &str, default_min: u64) -> Duration {
    let secs = std::env::var(var).ok().and_then(|v| v.parse().ok());
    Duration::from_secs(secs.unwrap_or(default_min * 60))
}

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_default().trim().to_string()
}

fn on_battery() -> bool {
    let Ok(dir) = fs::read_dir("/sys/class/power_supply") else { return false };
    let mut mains = false;
    for entry in dir.flatten() {
        let p = entry.path();
        if read(&format!("{}/type", p.display())) == "Mains" {
            mains = true;
            if read(&format!("{}/online", p.display())) == "1" {
                return false;
            }
        }
    }
    mains
}

/// "Fast Standard [Long_Life]" -> "Long_Life"
fn charge_mode() -> Option<String> {
    let s = read(&format!("{BAT}/charge_types"));
    let word = s.split_whitespace().find(|w| w.starts_with('['))?;
    Some(word.trim_matches(['[', ']']).to_string())
}

fn capacity() -> u32 {
    read(&format!("{BAT}/capacity")).parse().unwrap_or(0)
}

fn notify(icon: &str, title: &str, body: &str) {
    let Ok(bus) = Connection::new_session() else { return };
    let mut hints = PropMap::new();
    hints.insert("transient".into(), Variant(Box::new(true) as Box<dyn RefArg>));
    let proxy = bus.with_proxy("org.freedesktop.Notifications", "/org/freedesktop/Notifications", T);
    let _: Result<(u32,), _> = proxy.method_call(
        "org.freedesktop.Notifications",
        "Notify",
        ("Économie d'énergie", 0u32, icon, title, body, Vec::<String>::new(), hints, 5000i32),
    );
}

// Screen

fn rate(mode: &Value) -> f64 {
    mode["refreshRate"].as_f64().unwrap_or(0.0)
}

/// (output name, current mode id, wanted mode id) for the enabled built-in screen
fn screen_plan(battery: bool) -> Option<(String, String, String)> {
    let out = Command::new("kscreen-doctor").arg("-j").output().ok()?;
    let json: Value = serde_json::from_slice(&out.stdout).ok()?;
    // type 7 = Panel in libkscreen
    let panel = json["outputs"].as_array()?.iter().find(|o| o["type"] == 7 && o["enabled"] == true)?;
    let modes = panel["modes"].as_array()?;
    let current = modes.iter().find(|m| m["id"] == panel["currentModeId"])?;
    let same_size = modes.iter().filter(|m| m["size"] == current["size"]);
    let wanted = if battery {
        same_size.filter(|m| rate(m) >= 50.0).min_by(|a, b| (rate(a) - 60.0).abs().total_cmp(&(rate(b) - 60.0).abs()))?
    } else {
        same_size.max_by(|a, b| rate(a).total_cmp(&rate(b)))?
    };
    Some((panel["name"].as_str()?.to_string(), current["id"].as_str()?.to_string(), wanted["id"].as_str()?.to_string()))
}

fn set_screen(battery: bool) -> bool {
    let Some((name, current, wanted)) = screen_plan(battery) else { return false };
    if current == wanted {
        return true;
    }
    let arg = format!("output.{name}.mode.{wanted}");
    Command::new("kscreen-doctor").arg(arg).output().is_ok_and(|o| o.status.success())
}

// Bluetooth

/// true when an adapter is powered and no device is connected
fn bluetooth_idle(bus: &Connection) -> bool {
    let Ok(objects) = bus.with_proxy("org.bluez", "/", T).get_managed_objects() else { return false };
    let mut powered = false;
    for ifaces in objects.values() {
        if let Some(dev) = ifaces.get("org.bluez.Device1") {
            if prop_cast::<bool>(dev, "Connected") == Some(&true) {
                return false;
            }
        }
        if let Some(adapter) = ifaces.get("org.bluez.Adapter1") {
            powered |= prop_cast::<bool>(adapter, "Powered") == Some(&true);
        }
    }
    powered
}

/// Same as the Plasma toggle: soft-block every Bluetooth radio.
fn block_bluetooth() -> std::io::Result<()> {
    // struct rfkill_event { u32 idx; u8 type, op, soft, hard; }
    let mut ev = [0u8; 8];
    ev[4] = 2; // RFKILL_TYPE_BLUETOOTH
    ev[5] = 3; // RFKILL_OP_CHANGE_ALL
    ev[6] = 1;
    fs::OpenOptions::new().write(true).open("/dev/rfkill")?.write_all(&ev)
}

// Charge mode

fn back_to_conservation(full_since: &mut Option<Instant>, now: Instant, wait: Duration) {
    if charge_mode().is_none_or(|m| m == "Long_Life") || capacity() < 99 {
        *full_since = None;
        return;
    }
    let since = *full_since.get_or_insert(now);
    if now.duration_since(since) < wait {
        return;
    }
    *full_since = None;
    let ok = Command::new("sudo")
        .args(["-n", "/usr/local/bin/batterie-mode", "conservation"])
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        notify("security-high", "Conservation remise", "Batterie pleine et toujours branchée : la charge reste plafonnée pour la ménager.");
    }
}

fn watch(bus: &Connection, rule: MatchRule<'static>, flag: &Arc<AtomicBool>) -> Result<(), dbus::Error> {
    bus.add_match_no_cb(&rule.match_str())?;
    let flag = flag.clone();
    bus.start_receive(
        rule,
        Box::new(move |_, _| {
            flag.store(true, Ordering::Relaxed);
            true
        }),
    );
    Ok(())
}

fn run() -> Result<(), dbus::Error> {
    let bt_wait = delay("ECONOME_BT_DELAI", 10);
    let full_wait = delay("ECONOME_PLEIN_DELAI", 20);
    let bus = Connection::new_system()?;
    let power = Arc::new(AtomicBool::new(true));
    let bt = Arc::new(AtomicBool::new(true));

    let upower = MatchRule::new_signal(PROPS, "PropertiesChanged")
        .with_sender("org.freedesktop.UPower")
        .with_path("/org/freedesktop/UPower");
    watch(&bus, upower.static_clone(), &power)?;
    let bluez = MatchRule::new_signal(PROPS, "PropertiesChanged")
        .with_sender("org.bluez")
        .with_namespaced_path("/org/bluez");
    watch(&bus, bluez.static_clone(), &bt)?;
    for member in ["InterfacesAdded", "InterfacesRemoved"] {
        let rule = MatchRule::new_signal(MANAGER, member).with_sender("org.bluez");
        watch(&bus, rule.static_clone(), &bt)?;
    }

    let mut screen_for: Option<bool> = None;
    let mut next_poll: Option<Instant> = None;
    let mut full_since: Option<Instant> = None;
    let mut bt_check: Option<Instant> = None;
    let mut bt_deadline: Option<Instant> = None;

    loop {
        let now = Instant::now();
        if power.swap(false, Ordering::Relaxed) || next_poll.is_some_and(|t| t <= now) {
            let battery = on_battery();
            // kscreen may not be ready at login: retry on the next poll
            if screen_for != Some(battery) && set_screen(battery) {
                screen_for = Some(battery);
            }
            if battery {
                full_since = None;
            } else {
                back_to_conservation(&mut full_since, now, full_wait);
            }
            next_poll = (!battery || screen_for != Some(battery)).then(|| now + AC_POLL);
        }

        if bt.swap(false, Ordering::Relaxed) {
            bt_check.get_or_insert(now + BT_SETTLE);
        }
        if bt_check.is_some_and(|t| t <= now) {
            bt_check = None;
            if bluetooth_idle(&bus) {
                bt_deadline.get_or_insert(now + bt_wait);
            } else {
                bt_deadline = None;
            }
        }
        if bt_deadline.is_some_and(|t| t <= now) {
            bt_deadline = None;
            if bluetooth_idle(&bus) && block_bluetooth().is_ok() {
                let min = bt_wait.as_secs() / 60;
                notify("network-bluetooth-inactive", "Bluetooth coupé", &format!("Aucun appareil connecté depuis {min} min."));
            }
        }

        let wake = [next_poll, bt_check, bt_deadline].into_iter().flatten().min();
        let timeout = wake.map_or(Duration::from_secs(86400), |t| t.saturating_duration_since(Instant::now()));
        bus.process(timeout)?;
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] => {
            if let Err(e) = run() {
                eprintln!("econome : {e}");
                std::process::exit(1);
            }
        }
        ["ecran", which @ ("batterie" | "secteur")] => {
            std::process::exit(if set_screen(*which == "batterie") { 0 } else { 1 });
        }
        ["etat"] => {
            let bt = Connection::new_system().is_ok_and(|b| bluetooth_idle(&b));
            let screen = screen_plan(on_battery()).map(|(n, c, w)| format!("{n} mode {c} (voulu {w})"));
            println!("sur batterie : {}", if on_battery() { "oui" } else { "non" });
            println!("écran        : {}", screen.unwrap_or_else(|| "introuvable".into()));
            println!("charge       : {} ({} %)", charge_mode().unwrap_or_else(|| "?".into()), capacity());
            println!("bluetooth    : {}", if bt { "allumé, rien de connecté" } else { "coupé ou utilisé" });
        }
        _ => {
            eprintln!("usage : econome [ecran batterie|secteur | etat]");
            std::process::exit(2);
        }
    }
}
