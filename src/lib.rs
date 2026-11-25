//! experimental wifi sharing thing. no promises.
//!
//! it just drives iw/ip/hostapd/dnsmasq/iptables instead of reimplementing
//! them. easier to debug that way, and easier to throw away.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::process::{Command, Output};

pub const RUN_DIR: &str = "/run/airfork";
pub const STATE_FILE: &str = "/run/airfork/state.json";
pub const HOSTAPD_CONF: &str = "/run/airfork/hostapd.conf";
pub const DNSMASQ_CONF: &str = "/run/airfork/dnsmasq.conf";
pub const HOSTAPD_PID: &str = "/run/airfork/hostapd.pid";
pub const DNSMASQ_PID: &str = "/run/airfork/dnsmasq.pid";

// locally administered, so it never collides with the station MAC
pub const AP_MAC: &str = "02:00:00:00:01:01";
pub const GATEWAY: &str = "192.168.42.1";
pub const DHCP_RANGE: &str = "192.168.42.10,192.168.42.100,255.255.255.0,24h";
pub const NAT_COMMENT: &str = "airfork";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Band {
    B2_4,
    B5,
    B6,
}

impl Band {
    pub fn hostapd_mode(self) -> &'static str {
        match self {
            Band::B2_4 => "g",
            Band::B5 | Band::B6 => "a",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Band::B2_4 => "2.4 GHz",
            Band::B5 => "5 GHz",
            Band::B6 => "6 GHz",
        }
    }
}

// 2.4 is offset from 2407, the others start at 5000 and 5950.
// close enough for what hostapd needs.
pub fn frequency_to_channel(mhz: u32) -> Option<u8> {
    match mhz {
        2412..=2472 => Some(((mhz - 2407) / 5) as u8),
        2484 => Some(14),
        5000..=5895 => {
            let channel = (mhz - 5000) / 5;
            if (36..=196).contains(&channel) {
                Some(channel as u8)
            } else {
                None
            }
        }
        5950..=7125 => {
            let channel = (mhz - 5950) / 5;
            if (1..=253).contains(&channel) {
                Some(channel as u8)
            } else {
                None
            }
        }
        _ => None,
    }
}

pub fn band_for_frequency(mhz: u32) -> Option<Band> {
    match mhz {
        2412..=2484 => Some(Band::B2_4),
        5000..=5895 => Some(Band::B5),
        5950..=7125 => Some(Band::B6),
        _ => None,
    }
}

#[derive(Debug, Clone)]
struct IwDevEntry {
    phy: String,
    name: String,
    kind: String,
    ssid: Option<String>,
    freq: Option<u32>,
}

// iw dev is indent-based. walk the lines, keep whatever block is open.
fn parse_iw_dev(text: &str) -> Vec<IwDevEntry> {
    let mut entries = Vec::new();
    let mut phy = String::new();
    let mut current: Option<IwDevEntry> = None;

    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("phy#") {
            phy = format!("phy{rest}");
            continue;
        }
        if let Some(name) = line.strip_prefix("Interface ") {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(IwDevEntry {
                phy: phy.clone(),
                name: name.to_string(),
                kind: String::new(),
                ssid: None,
                freq: None,
            });
            continue;
        }
        if line.starts_with("Unnamed/non-netdev interface") {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(IwDevEntry {
                phy: phy.clone(),
                name: "<p2p>".into(),
                kind: String::new(),
                ssid: None,
                freq: None,
            });
            continue;
        }
        if let Some(entry) = current.as_mut() {
            if let Some(value) = line.strip_prefix("type ") {
                entry.kind = value.to_string();
            } else if let Some(value) = line.strip_prefix("ssid ") {
                entry.ssid = Some(value.to_string());
            } else if line.starts_with("channel ") {
                entry.freq = line
                    .split('(')
                    .nth(1)
                    .and_then(|part| part.split_whitespace().next())
                    .and_then(|value| value.parse::<u32>().ok());
            }
        }
    }
    if let Some(entry) = current {
        entries.push(entry);
    }
    entries
}

// glue each "* #{ ... }" block and its "total ... #channels <=" continuation
// into one line. managed and AP have to be in the same block, that's the trick.
fn parse_combinations(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut in_section = false;
    let mut current = String::new();

    for line in text.lines() {
        if line.contains("valid interface combinations:") {
            in_section = true;
            continue;
        }
        if !in_section {
            continue;
        }
        let line = line.trim();
        if line.starts_with('*') {
            if !current.is_empty() {
                blocks.push(current.clone());
            }
            current = line.to_string();
        } else if line.starts_with('#') || line.starts_with("total") {
            current.push(' ');
            current.push_str(line);
        } else if line.is_empty() {
            continue;
        } else {
            break;
        }
    }
    if !current.is_empty() {
        blocks.push(current);
    }
    blocks
}

// dig the number out of "#channels <= 1"
fn max_channels(block: &str) -> Option<u32> {
    let marker = "#channels <=";
    let index = block.find(marker)?;
    block[index + marker.len()..]
        .trim()
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    pub phy: String,
    pub uplink: String,
    pub uplink_ssid: Option<String>,
    pub frequency_mhz: Option<u32>,
    pub channel: Option<u8>,
    pub band: Option<Band>,
    pub concurrent: bool,
    pub same_channel_required: bool,
    pub p2p_device: bool,
    pub combinations: Vec<String>,
    pub missing_tools: Vec<String>,
}

fn tool_available(name: &str) -> bool {
    if let Some(paths) = env::var_os("PATH") {
        for directory in env::split_paths(&paths) {
            if directory.join(name).is_file() {
                return true;
            }
        }
    }
    false
}

pub fn is_root() -> bool {
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(uid) = line.strip_prefix("Uid:") {
            return uid.split_whitespace().next() == Some("0");
        }
    }
    false
}

fn command_output(program: &str, args: &[&str]) -> Result<Output> {
    Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("could not execute {program}"))
}

fn stdout(program: &str, args: &[&str]) -> Result<String> {
    let output = command_output(program, args)?;
    if !output.status.success() {
        bail!(
            "{} {} failed: {}",
            program,
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run(program: &str, args: &[&str]) -> Result<()> {
    let output = command_output(program, args)?;
    if !output.status.success() {
        bail!(
            "{} {} failed: {}",
            program,
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

// read-only. safe to run as a normal user.
pub fn detect() -> Result<Capability> {
    let devices = parse_iw_dev(&stdout("iw", &["dev"])?);
    let station = devices
        .iter()
        .find(|entry| entry.kind == "managed" && entry.freq.is_some())
        .or_else(|| devices.iter().find(|entry| entry.kind == "managed"))
        .ok_or_else(|| anyhow!("no managed Wi-Fi interface found; connect to a network first"))?;

    let info = stdout("iw", &["phy", &station.phy, "info"])?;
    let combinations = parse_combinations(&info);
    let concurrent = combinations
        .iter()
        .any(|block| block.contains("managed") && block.contains("AP"));
    let same_channel_required = combinations
        .iter()
        .filter(|block| block.contains("managed") && block.contains("AP"))
        .all(|block| max_channels(block) == Some(1));

    let missing_tools = ["iw", "ip", "hostapd", "dnsmasq", "iptables"]
        .iter()
        .filter(|tool| !tool_available(tool))
        .map(|tool| (*tool).to_string())
        .collect();

    let ssid = station
        .ssid
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    Ok(Capability {
        phy: station.phy.clone(),
        uplink: station.name.clone(),
        uplink_ssid: ssid,
        frequency_mhz: station.freq,
        channel: station.freq.and_then(frequency_to_channel),
        band: station.freq.and_then(band_for_frequency),
        concurrent,
        same_channel_required,
        p2p_device: devices.iter().any(|entry| entry.kind == "P2P-device"),
        combinations,
        missing_tools,
    })
}

#[derive(Debug, Clone)]
pub struct HotspotConfig {
    pub ssid: String,
    pub password: String,
    pub ap_interface: String,
    pub country: String,
}

impl Default for HotspotConfig {
    fn default() -> Self {
        Self {
            ssid: "AirforkLab".into(),
            password: "airfork-2026".into(),
            ap_interface: "wlan0_ap".into(),
            country: "KE".into(),
        }
    }
}

impl HotspotConfig {
    // mostly to keep newlines out of generated config files
    pub fn validate(&self) -> Result<()> {
        if self.ssid.is_empty() || self.ssid.len() > 32 {
            bail!("SSID must be between 1 and 32 bytes");
        }
        if self.password.len() < 8 {
            bail!("password must be at least 8 characters for WPA2");
        }
        if self.ssid.contains('\n') || self.password.contains('\n') {
            bail!("SSID and password must not contain newlines");
        }
        if self.ap_interface.is_empty()
            || !self
                .ap_interface
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            bail!("AP interface name may contain only letters, digits and underscores");
        }
        if self.country.len() != 2 || !self.country.chars().all(|c| c.is_ascii_alphanumeric()) {
            bail!("country code must be two ASCII alphanumeric characters");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeState {
    pub uplink: String,
    pub ap_interface: String,
    pub channel: u8,
    pub band: String,
    pub ssid: String,
    pub gateway: String,
    pub hostapd_pid: u32,
    pub dnsmasq_pid: u32,
    pub ip_forward_before: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusInfo {
    pub running: bool,
    pub state: Option<RuntimeState>,
    pub capability: Option<Capability>,
}

fn render_hostapd_conf(capability: &Capability, config: &HotspotConfig) -> Result<String> {
    let channel = capability
        .channel
        .context("the uplink has no channel; connect to a Wi-Fi network first")?;
    let band = capability
        .band
        .context("the uplink frequency could not be classified")?;
    Ok(format!(
        "interface={}\ndriver=nl80211\nssid={}\nhw_mode={}\nchannel={}\ncountry_code={}\nwpa=2\nwpa_passphrase={}\nwpa_key_mgmt=WPA-PSK\nrsn_pairwise=CCMP\n",
        config.ap_interface,
        config.ssid,
        band.hostapd_mode(),
        channel,
        config.country,
        config.password
    ))
}

fn render_dnsmasq_conf(config: &HotspotConfig) -> String {
    format!(
        "interface={}\nbind-interfaces\ndhcp-range={}\npid-file={}\n",
        config.ap_interface, DHCP_RANGE, DNSMASQ_PID
    )
}

fn read_pid(path: &str) -> Result<u32> {
    let contents = fs::read_to_string(path).with_context(|| format!("could not read {path}"))?;
    contents
        .trim()
        .parse::<u32>()
        .with_context(|| format!("invalid PID in {path}"))
}

fn process_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

pub fn plan(config: &HotspotConfig) -> Result<String> {
    config.validate()?;
    let capability = detect()?;
    if !capability.concurrent {
        bail!(
            "this adapter cannot run station and AP interfaces together; the combinations are:\n{}",
            capability.combinations.join("\n")
        );
    }
    let channel = capability
        .channel
        .context("the uplink has no channel; connect to a Wi-Fi network first")?;
    let band = capability
        .band
        .context("the uplink frequency could not be classified")?;
    Ok(format!(
        "Uplink       {} on {} channel {} ({} MHz, {})\n\
         Virtual AP  {} with MAC {}\n\
         Hotspot     \"{}\" on {} channel {} (WPA2)\n\
         Network     192.168.42.0/24, DHCP 192.168.42.10 through 100\n\
         Sharing     NAT from {} to {} via iptables\n\
         Cleanup     airfork stop removes interface, daemons, NAT rule and state\n\
         Note        one radio, so the two roles share throughput\n",
        capability.uplink,
        capability.uplink_ssid.as_deref().unwrap_or("<hidden>"),
        channel,
        capability.frequency_mhz.unwrap_or(0),
        band.label(),
        config.ap_interface,
        AP_MAC,
        config.ssid,
        band.label(),
        channel,
        config.ap_interface,
        capability.uplink
    ))
}

// the actual work. everything here is reversible by stop().
pub fn start(config: &HotspotConfig) -> Result<RuntimeState> {
    config.validate()?;
    if !is_root() {
        bail!("start needs root; use sudo or pkexec");
    }

    let capability = detect()?;
    if !capability.concurrent {
        bail!(
            "this adapter cannot run station and AP interfaces together; the combinations are:\n{}",
            capability.combinations.join("\n")
        );
    }
    if !capability.missing_tools.is_empty() {
        bail!(
            "missing required tools: {}",
            capability.missing_tools.join(", ")
        );
    }
    if Path::new(STATE_FILE).exists() {
        stop().context("could not clear the previous session")?;
    }

    let channel = capability
        .channel
        .context("the uplink has no channel; connect to a Wi-Fi network first")?;
    let band = capability
        .band
        .context("the uplink frequency could not be classified")?;

    fs::create_dir_all(RUN_DIR)?;
    fs::write(HOSTAPD_CONF, render_hostapd_conf(&capability, config)?)?;
    fs::write(DNSMASQ_CONF, render_dnsmasq_conf(config))?;

    let forward_before = fs::read_to_string("/proc/sys/net/ipv4/ip_forward")
        .unwrap_or_else(|_| "0\n".to_string())
        .trim()
        .to_string();

    run(
        "iw",
        &[
            "dev",
            &capability.uplink,
            "interface",
            "add",
            &config.ap_interface,
            "type",
            "__ap",
        ],
    )
    .context("could not create the virtual AP interface")?;

    // anything after the interface creation can fail, so undo on the way out
    let result = (|| -> Result<RuntimeState> {
        run(
            "ip",
            &["link", "set", &config.ap_interface, "address", AP_MAC],
        )?;
        let _ = run("nmcli", &["dev", "set", &config.ap_interface, "managed", "no"]);

        run("hostapd", &["-B", "-P", HOSTAPD_PID, HOSTAPD_CONF])?;
        let hostapd_pid = read_pid(HOSTAPD_PID)?;

        run(
            "ip",
            &[
                "addr",
                "add",
                &format!("{GATEWAY}/24"),
                "dev",
                &config.ap_interface,
            ],
        )?;
        run("dnsmasq", &["-C", DNSMASQ_CONF])?;
        let dnsmasq_pid = read_pid(DNSMASQ_PID)?;

        // comment on the rule so stop() can find exactly this one
        let nat_args = [
            "-t",
            "nat",
            "-C",
            "POSTROUTING",
            "-o",
            &capability.uplink,
            "-j",
            "MASQUERADE",
            "-m",
            "comment",
            "--comment",
            NAT_COMMENT,
        ];
        let already_natted = Command::new("iptables")
            .args(nat_args)
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if !already_natted {
            let mut add_args = nat_args.to_vec();
            add_args[2] = "-A";
            run("iptables", &add_args)?;
        }

        fs::write("/proc/sys/net/ipv4/ip_forward", "1\n")?;

        Ok(RuntimeState {
            uplink: capability.uplink.clone(),
            ap_interface: config.ap_interface.clone(),
            channel,
            band: band.label().to_string(),
            ssid: config.ssid.clone(),
            gateway: GATEWAY.to_string(),
            hostapd_pid,
            dnsmasq_pid,
            ip_forward_before: forward_before.clone(),
        })
    })();

    match result {
        Ok(state) => {
            fs::write(STATE_FILE, serde_json::to_string_pretty(&state)?)?;
            Ok(state)
        }
        Err(error) => {
            for pidfile in [HOSTAPD_PID, DNSMASQ_PID] {
                if let Ok(pid) = read_pid(pidfile) {
                    let _ = run("kill", &[&pid.to_string()]);
                }
                let _ = fs::remove_file(pidfile);
            }
            let _ = run(
                "iptables",
                &[
                    "-t",
                    "nat",
                    "-D",
                    "POSTROUTING",
                    "-o",
                    &capability.uplink,
                    "-j",
                    "MASQUERADE",
                    "-m",
                    "comment",
                    "--comment",
                    NAT_COMMENT,
                ],
            );
            let _ = fs::write(
                "/proc/sys/net/ipv4/ip_forward",
                format!("{}\n", forward_before),
            );
            let _ = run("iw", &["dev", &config.ap_interface, "del"]);
            Err(error)
        }
    }
}

pub fn stop() -> Result<()> {
    if !is_root() {
        bail!("stop needs root; use sudo or pkexec");
    }
    let contents = match fs::read_to_string(STATE_FILE) {
        Ok(value) => value,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            bail!("no active airfork session")
        }
        Err(error) => return Err(error.into()),
    };
    let state: RuntimeState = serde_json::from_str(&contents).context("invalid session state")?;

    if process_alive(state.hostapd_pid) {
        let _ = run("kill", &[&state.hostapd_pid.to_string()]);
    }
    if process_alive(state.dnsmasq_pid) {
        let _ = run("kill", &[&state.dnsmasq_pid.to_string()]);
    }
    let _ = run(
        "iptables",
        &[
            "-t",
            "nat",
            "-D",
            "POSTROUTING",
            "-o",
            &state.uplink,
            "-j",
            "MASQUERADE",
            "-m",
            "comment",
            "--comment",
            NAT_COMMENT,
        ],
    );
    let _ = fs::write(
        "/proc/sys/net/ipv4/ip_forward",
        format!("{}\n", state.ip_forward_before),
    );
    let _ = run("iw", &["dev", &state.ap_interface, "del"]);
    let _ = fs::remove_file(STATE_FILE);
    Ok(())
}

pub fn status() -> StatusInfo {
    let state = fs::read_to_string(STATE_FILE)
        .ok()
        .and_then(|contents| serde_json::from_str::<RuntimeState>(&contents).ok());
    let running = state
        .as_ref()
        .map(|state| process_alive(state.hostapd_pid) && process_alive(state.dnsmasq_pid))
        .unwrap_or(false);
    StatusInfo {
        running,
        state,
        capability: detect().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_from_frequencies() {
        assert_eq!(frequency_to_channel(2412), Some(1));
        assert_eq!(frequency_to_channel(2472), Some(13));
        assert_eq!(frequency_to_channel(2484), Some(14));
        assert_eq!(frequency_to_channel(5220), Some(44));
        assert_eq!(frequency_to_channel(5955), Some(1));
        assert_eq!(frequency_to_channel(100), None);
    }

    #[test]
    fn bands_from_frequencies() {
        assert_eq!(band_for_frequency(2437), Some(Band::B2_4));
        assert_eq!(band_for_frequency(5220), Some(Band::B5));
        assert_eq!(band_for_frequency(5955), Some(Band::B6));
        assert_eq!(band_for_frequency(100), None);
    }

    #[test]
    fn iw_dev_blocks() {
        let sample = "phy#0\n\tInterface wlan0\n\t\tifindex 3\n\t\ttype managed\n\t\tssid Lab\n\t\tchannel 44 (5220 MHz), width: 80 MHz\n";
        let entries = parse_iw_dev(sample);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].phy, "phy0");
        assert_eq!(entries[0].name, "wlan0");
        assert_eq!(entries[0].kind, "managed");
        assert_eq!(entries[0].freq, Some(5220));
        assert_eq!(entries[0].ssid.as_deref(), Some("Lab"));
    }

    #[test]
    fn iwlwifi_combinations() {
        let sample = "\
\tvalid interface combinations:
\t\t * #{ managed } <= 1, #{ P2P-client, P2P-GO } <= 1, #{ P2P-device } <= 1,
\t\t   total <= 3, #channels <= 2
\t\t * #{ managed } <= 1, #{ AP, P2P-client, P2P-GO } <= 1, #{ P2P-device } <= 1,
\t\t   total <= 3, #channels <= 1
\tHT Capability overrides:
";
        let blocks = parse_combinations(sample);
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0].contains("managed") && !blocks[0].contains("AP"));
        assert!(blocks[1].contains("managed") && blocks[1].contains("AP"));
        assert_eq!(max_channels(&blocks[1]), Some(1));
    }

    #[test]
    fn config_checks() {
        let mut config = HotspotConfig::default();
        assert!(config.validate().is_ok());
        config.ssid = "x".repeat(33);
        assert!(config.validate().is_err());
        config.ssid = "lab".into();
        config.password = "short".into();
        assert!(config.validate().is_err());
        config.password = "longenough".into();
        config.ap_interface = "bad-name".into();
        assert!(config.validate().is_err());
    }
}
