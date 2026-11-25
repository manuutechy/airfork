# airfork

Experimental Linux tool that forks one Wi-Fi radio into two: it keeps the
station connection up and puts a virtual access point next to it, so other
devices can share the uplink. No second adapter needed on cards like the
Intel 9260 / AX200 where `iwlwifi` allows `managed` + `AP` at the same time.

This is a toy for lab use. Expect rough edges.

## the catch

On `iwlwifi` the two interfaces have to sit on the same channel. That means
the hotspot channel follows the uplink channel, not the other way round. If
the uplink moves, stop and start again. Throughput is shared between the two
roles because there is only one transmitter.

`nmcli dev wifi hotspot` cannot do this, it takes the card over and drops
the station connection. airfork adds a virtual `__ap` interface and runs
hostapd and dnsmasq on it instead, which is the trick Windows hides behind
its Mobile Hotspot toggle.

## build and install

```sh
cargo build --release
./install.sh
```

`install.sh` drops the cli in `/usr/local/bin`, the gui in `~/.local/bin`, a
desktop launcher, and a polkit action so pkexec asks once per session instead
of on every button press. Needs `iw`, `ip`, `hostapd`, `dnsmasq`, `iptables`
and NetworkManager.

## use

```sh
./target/release/airfork detect
./target/release/airfork plan
sudo ./target/release/airfork start --ssid LabHotspot --password somethinglong
./target/release/airfork status
sudo ./target/release/airfork stop
```

`detect` is read only and works as a normal user. `start` and `stop` need
root. `stop` removes the virtual interface, kills the two daemons, drops the
NAT rule and restores `ip_forward`.

There is also `airfork-gui` with the same buttons for people who would
rather click than type. It shells out through `pkexec` for privileged bits.

## why the tool exists

Because the capability is already in the driver and the missing piece is a
few lines of glue. The whole thing just reads `iw phy ... info`, checks that
`managed` and `AP` appear in one interface-combination block, then wires up
the same channel and hands off to the normal Linux networking stack.
