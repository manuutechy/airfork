//! tiny gtk window for people who don't like typing
//!
//! read-only bits call the library directly. start/stop shell out to the cli
//! through pkexec, so the gui itself stays unprivileged.

use anyhow::{Context, Result};
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{Box, Button, Entry, Label, Orientation, ScrolledWindow, TextView, WrapMode};
use libadwaita as adw;
use adw::prelude::*;

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use airfork::{status, HotspotConfig, StatusInfo};

enum Msg {
    Refresh(StatusInfo),
    Started(Result<String, String>),
    Stopped(Result<(), String>),
}

type Queue = Arc<Mutex<Vec<Msg>>>;

fn main() {
    let app = adw::Application::builder()
        .application_id("dev.airfork.Gui")
        .build();
    app.connect_activate(build_ui);
    app.run();
}

fn log(view: &TextView, line: &str) {
    let buffer = view.buffer();
    let mut end = buffer.end_iter();
    buffer.insert(&mut end, &format!("{line}\n"));
}

fn cli_path() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let sibling = exe
        .parent()
        .context("no parent directory for the gui binary")?
        .join("airfork");
    if sibling.exists() {
        return Ok(sibling);
    }
    Ok(PathBuf::from("airfork"))
}

// pkexec prompts on a desktop, which beats typing a sudo password into a box
fn privileged(args: &[String]) -> Result<String> {
    let cli = cli_path()?;
    let mut command = if airfork::is_root() {
        Command::new(&cli)
    } else {
        let mut command = Command::new("pkexec");
        command.arg(&cli);
        command
    };
    let output = command.args(args).output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() {
        Ok(stdout)
    } else {
        anyhow::bail!("{}", if stderr.is_empty() { stdout } else { stderr })
    }
}

fn handle_msg(msg: Msg, detail: &Label, log_view: &TextView) {
    match msg {
        Msg::Refresh(info) => {
            let mut lines = Vec::new();
            match &info.capability {
                Some(cap) => {
                    lines.push(format!(
                        "{} / {} — {}",
                        cap.phy,
                        cap.uplink,
                        cap.uplink_ssid.as_deref().unwrap_or("<hidden>")
                    ));
                    match (cap.channel, cap.frequency_mhz, cap.band) {
                        (Some(ch), Some(freq), Some(band)) => lines.push(format!(
                            "uplink: channel {ch} ({freq} MHz, {})",
                            band.label()
                        )),
                        _ => lines.push("uplink: not connected".into()),
                    }
                    lines.push(format!(
                        "concurrent AP+STA: {}",
                        if cap.concurrent { "yes" } else { "no" }
                    ));
                    lines.push(format!(
                        "same channel required: {}",
                        if cap.same_channel_required { "yes" } else { "no" }
                    ));
                    for combination in &cap.combinations {
                        lines.push(format!("  {combination}"));
                    }
                }
                None => lines.push("adapter unavailable".into()),
            }
            match &info.state {
                Some(state) => lines.push(format!(
                    "session: {} ({} on {})",
                    if info.running { "running" } else { "stale" },
                    state.ssid,
                    state.ap_interface
                )),
                None => lines.push("session: not running".into()),
            }
            detail.set_label(&lines.join("\n"));
        }
        Msg::Started(result) => match result {
            Ok(text) => {
                log(log_view, "started");
                for line in text.lines() {
                    log(log_view, line);
                }
            }
            Err(error) => log(log_view, &format!("start failed: {error}")),
        },
        Msg::Stopped(result) => match result {
            Ok(()) => log(log_view, "stopped"),
            Err(error) => log(log_view, &format!("stop failed: {error}")),
        },
    }
}

fn build_ui(app: &adw::Application) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("airfork — experimental wifi share")
        .default_width(560)
        .default_height(640)
        .build();

    let root = Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(10)
        .margin_start(16)
        .margin_end(16)
        .margin_top(16)
        .margin_bottom(16)
        .build();

    let headline = Label::builder().label("one radio, two jobs").xalign(0.0).build();
    headline.add_css_class("title-1");

    let detail = Label::builder()
        .label("checking adapter...")
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .build();

    let ssid = Entry::builder().placeholder_text("hotspot ssid").build();
    ssid.set_text("LabHotspot");
    let password = Entry::builder().placeholder_text("wpa2 password").build();
    password.set_text("airfork-2026");
    password.set_visibility(false);
    let iface = Entry::builder().placeholder_text("virtual interface").build();
    iface.set_text("wlan0_ap");

    let buttons = Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .build();
    let start = Button::with_label("start");
    let stop = Button::with_label("stop");
    let refresh = Button::with_label("refresh");
    buttons.append(&start);
    buttons.append(&stop);
    buttons.append(&refresh);

    let log_view = TextView::builder()
        .editable(false)
        .wrap_mode(WrapMode::Word)
        .build();
    log_view.set_height_request(170);
    let scroller = ScrolledWindow::builder()
        .child(&log_view)
        .min_content_height(170)
        .build();

    root.append(&headline);
    root.append(&detail);
    root.append(&Label::builder().label("hotspot name").xalign(0.0).build());
    root.append(&ssid);
    root.append(&Label::builder().label("password (8+ characters)").xalign(0.0).build());
    root.append(&password);
    root.append(&Label::builder().label("virtual interface name").xalign(0.0).build());
    root.append(&iface);
    root.append(&buttons);
    root.append(&scroller);
    window.set_content(Some(&root));
    window.present();

    // no glib channel in 0.22, so a plain shared vec and an idle callback
    let queue: Queue = Arc::new(Mutex::new(Vec::new()));
    let ui_queue = queue.clone();
    let ui_detail = detail.clone();
    let ui_log = log_view.clone();
    glib::source::idle_add_local(move || {
        let mut items = ui_queue.lock().unwrap();
        while let Some(msg) = items.pop() {
            handle_msg(msg, &ui_detail, &ui_log);
        }
        drop(items);
        glib::ControlFlow::Continue
    });

    let refresh_queue = queue.clone();
    refresh.connect_clicked(move |_| {
        refresh_queue.lock().unwrap().push(Msg::Refresh(status()));
    });

    let start_queue = queue.clone();
    let ssid_value = ssid.text().to_string();
    let pass_value = password.text().to_string();
    let iface_value = iface.text().to_string();
    start.connect_clicked(move |_| {
        let config = HotspotConfig {
            ssid: ssid_value.clone(),
            password: pass_value.clone(),
            ap_interface: iface_value.clone(),
            country: "KE".into(),
        };
        if let Err(error) = config.validate() {
            log(&log_view, &format!("bad config: {error}"));
            return;
        }
        let queue = start_queue.clone();
        std::thread::spawn(move || {
            let args = vec![
                "start".to_string(),
                "--ssid".to_string(),
                config.ssid.clone(),
                "--password".to_string(),
                config.password.clone(),
                "--ap-interface".to_string(),
                config.ap_interface.clone(),
            ];
            let result = privileged(&args).map_err(|error| error.to_string());
            queue.lock().unwrap().push(Msg::Started(result));
            queue.lock().unwrap().push(Msg::Refresh(status()));
        });
    });

    let stop_queue = queue.clone();
    stop.connect_clicked(move |_| {
        let queue = stop_queue.clone();
        std::thread::spawn(move || {
            let result = privileged(&["stop".to_string()])
                .map(|_| ())
                .map_err(|error| error.to_string());
            queue.lock().unwrap().push(Msg::Stopped(result));
            queue.lock().unwrap().push(Msg::Refresh(status()));
        });
    });

    let initial_queue = queue.clone();
    std::thread::spawn(move || {
        initial_queue.lock().unwrap().push(Msg::Refresh(status()));
    });
}
