// perf-retour: Performance picked by hand, then forgotten. Once the CPU has been
// quiet for a while, go back to Balanced on AC, Power Saver on battery.
//
// It sleeps on the system bus (power-profiles-daemon). Only while Performance is
// active does it wake up, every 30 s, to read one line of /proc/stat.
// A program holding Performance (a game, gamemode...) is left alone.
//
// Command line: perf-retour         daemon
//               perf-retour etat
// Tuning (env): PERF_RETOUR_DELAI seconds of quiet (300), PERF_RETOUR_SEUIL
//               % of all cores below which it is quiet (25), PERF_RETOUR_PAS (30)

use dbus::arg::{PropMap, RefArg, Variant};
use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;
use dbus::blocking::{Connection, Proxy};
use dbus::channel::MatchingReceiver;
use dbus::message::MatchRule;
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PPD: &str = "org.freedesktop.UPower.PowerProfiles";
const PPD_PATH: &str = "/org/freedesktop/UPower/PowerProfiles";
const PROPS: &str = "org.freedesktop.DBus.Properties";
const T: Duration = Duration::from_secs(5);

struct Settings {
    quiet_for: Duration,
    threshold: f64,
    step: Duration,
}

fn env_num(var: &str, default: f64) -> f64 {
    std::env::var(var).ok().and_then(|v| v.parse().ok()).filter(|v: &f64| *v > 0.0).unwrap_or(default)
}

impl Settings {
    fn from_env() -> Self {
        Settings {
            quiet_for: Duration::from_secs_f64(env_num("PERF_RETOUR_DELAI", 300.0)),
            threshold: env_num("PERF_RETOUR_SEUIL", 25.0),
            step: Duration::from_secs_f64(env_num("PERF_RETOUR_PAS", 30.0)),
        }
    }
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

/// (busy, total) jiffies over all cores, first line of /proc/stat
fn cpu_times() -> Option<(u64, u64)> {
    let stat = fs::read_to_string("/proc/stat").ok()?;
    let v: Vec<u64> = stat.lines().next()?.split_whitespace().skip(1).take(8).filter_map(|x| x.parse().ok()).collect();
    if v.len() < 5 {
        return None;
    }
    let total: u64 = v.iter().sum();
    Some((total - v[3] - v[4], total))
}

fn ppd(bus: &Connection) -> Proxy<'_, &Connection> {
    bus.with_proxy(PPD, PPD_PATH, T)
}

fn profile(bus: &Connection) -> Option<String> {
    ppd(bus).get::<String>(PPD, "ActiveProfile").ok()
}

fn held(bus: &Connection) -> bool {
    ppd(bus).get::<Vec<PropMap>>(PPD, "ActiveProfileHolds").is_ok_and(|h| !h.is_empty())
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

fn human(d: Duration) -> String {
    match d.as_secs() {
        s if s < 60 => format!("{s} s"),
        s => format!("{} min", s / 60),
    }
}

fn leave_performance(bus: &Connection, quiet_for: Duration) {
    let (target, icon, name) = if on_battery() {
        ("power-saver", "battery-profile-powersave", "Économie")
    } else {
        ("balanced", "battery-profile-balanced", "Équilibré")
    };
    match ppd(bus).set(PPD, "ActiveProfile", target.to_string()) {
        Ok(()) => notify(icon, &format!("Retour en {name}"), &format!("Performance sans usage depuis {}.", human(quiet_for))),
        Err(e) => eprintln!("perf-retour : impossible de passer en {target} : {e}"),
    }
}

fn run() -> Result<(), dbus::Error> {
    let s = Settings::from_env();
    let bus = Connection::new_system()?;
    let changed = Arc::new(AtomicBool::new(true));

    let rule = MatchRule::new_signal(PROPS, "PropertiesChanged").with_sender(PPD).with_path(PPD_PATH);
    bus.add_match_no_cb(&rule.match_str())?;
    let flag = changed.clone();
    bus.start_receive(
        rule.static_clone(),
        Box::new(move |_, _| {
            flag.store(true, Ordering::Relaxed);
            true
        }),
    );

    let mut watching = false;
    let mut last: Option<(Instant, u64, u64)> = None;
    let mut quiet_since: Option<Instant> = None;

    loop {
        if changed.swap(false, Ordering::Relaxed) {
            let perf = profile(&bus).as_deref() == Some("performance");
            if perf != watching {
                watching = perf;
                quiet_since = None;
                last = cpu_times().map(|(b, t)| (Instant::now(), b, t));
            }
        }

        let now = Instant::now();
        if let Some((at, busy0, total0)) = last.filter(|_| watching) {
            if now >= at + s.step {
                // iowait can go backwards in /proc/stat, hence the saturating subs
                let (busy, total) = cpu_times().unwrap_or((busy0, total0));
                let load = 100.0 * busy.saturating_sub(busy0) as f64 / total.saturating_sub(total0).max(1) as f64;
                last = Some((now, busy, total));
                if total > total0 && load < s.threshold && !held(&bus) {
                    let since = *quiet_since.get_or_insert(at);
                    if now - since >= s.quiet_for {
                        quiet_since = None;
                        leave_performance(&bus, s.quiet_for);
                    }
                } else {
                    quiet_since = None;
                }
            }
        }

        let timeout = match last.filter(|_| watching) {
            Some((at, _, _)) => (at + s.step).saturating_duration_since(Instant::now()),
            None => Duration::from_secs(86400),
        };
        bus.process(timeout)?;
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] => {
            if let Err(e) = run() {
                eprintln!("perf-retour : {e}");
                std::process::exit(1);
            }
        }
        ["etat"] => {
            let s = Settings::from_env();
            let bus = Connection::new_system().ok();
            let prof = bus.as_ref().and_then(profile).unwrap_or_else(|| "?".into());
            let hold = bus.as_ref().is_some_and(held);
            println!("profil   : {prof}{}", if hold { " (réservé par un programme)" } else { "" });
            println!("retour   : après {} sous {} % de CPU, vers {}", human(s.quiet_for), s.threshold,
                if on_battery() { "Économie (batterie)" } else { "Équilibré (secteur)" });
        }
        _ => {
            eprintln!("usage : perf-retour [etat]");
            std::process::exit(2);
        }
    }
}
