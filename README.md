# airfork

Experimental Linux tool that shares one Wi-Fi card between an uplink and a
hotspot. Tested on the **Intel Wireless-AC 9260** with the `iwlwifi` driver.
Open to anyone who wants to use or change it. Built by Meshack Bahati.

![airfork GUI](screenshots/gui-cropped.png)

## Why

Windows hides this behind one Mobile Hotspot toggle. On Linux the usual
`nmcli dev wifi hotspot` takes the card over and drops the existing Wi-Fi
connection, so most guides tell you to buy a second adapter.

The Intel 9260 already permits one station and one AP interface at the same
time. airfork supplies the missing glue: it checks the driver combinations,
keeps NetworkManager off the virtual interface, works around the passive-scan
rule on 5 GHz, ignores the router's band-steering requests during a session,
and wires up hostapd, dnsmasq, NAT and policy routing. It all comes down again
on `stop`.

The GUI shows the internet source, connected clients and their leased
addresses.

## Build

Needs `iw`, `ip`, `hostapd`, `dnsmasq`, `iptables`, NetworkManager and GTK4 /
libadwaita for the GUI.

```sh
cargo build --release
./install.sh
```

`install.sh` puts the CLI in `/usr/local/bin`, the GUI in `~/.local/bin`, adds
a desktop launcher and a polkit action so the privileged buttons ask once per
session.

## Use

```sh
airfork detect
airfork sources
airfork plan --uplink wlan0
sudo airfork start --ssid LabHotspot --password airfork-2026 --uplink wlan0
airfork status
sudo airfork stop
```

The GUI has the same controls. Choose the internet source from the dropdown
before starting, then press **Start hotspot**. The **Clients** group refreshes
every two seconds with joined devices and their addresses. Defaults are
`LabHotspot` and `airfork-2026`.

## Limits

One radio means one channel. On the Intel 9260 the driver reports
`#channels <= 1` for station plus AP, and every 5 GHz channel is passive-scan.
Starting from 5 GHz therefore moves the station to 2.4 GHz once. The link drops
and returns after roughly 40 seconds while NetworkManager renews its address.
Later starts do not repeat it. Choosing Ethernet as the internet source does
not remove this radio constraint.

Throughput is shared between the uplink and the clients. The connection
profile is limited to band `bg` during a session and restored on stop; the
station remains on 2.4 GHz until it reconnects on its own. 802.11v transition
handling is disabled during a session so the router cannot steer the station
back to 5 GHz. This is a lab tool for sharing your own connection, not a
jamming tool.

## Layout

```
src/lib.rs   detection, start, stop, clients, rollback
src/cli.rs   command-line frontend
src/gui.rs   GTK frontend
```

## License

MIT. Free to use, copy, modify and share. Copyright (c) 2026 Meshack Bahati.
See `LICENSE`.
