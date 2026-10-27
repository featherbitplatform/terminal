---
id: tunnels
title: Tunnels
sidebar_label: Tunnels
---

A tunnel is a named port forward or SOCKS proxy that rides an SSH profile's connection. Use one to reach a database on a private host, a web server behind a bastion, or to let a remote machine reach a service on yours. Tunnels belong to the profile, so they go through its [jump hosts](profiles#jump-hosts) too.

![The Tunnels window: tunnels grouped by profile, each with its addresses, state and an on/off switch](/img/tunnels-panel.png)

## Kinds

| Kind | What it does |
| --- | --- |
| **Local** | Listens on your machine and sends each connection to a target address as seen from the profile's host. `localhost:5432` forwarded to `db.internal:5432` lets a local client reach a database that only the host can see. |
| **Remote** | Listens on the profile's host and sends each connection to a target address as seen from your machine. The server address and port are on the host; the target is on your side. |
| **SOCKS** | Listens on your machine as a SOCKS5 proxy. Each connection a client asks for is opened from the profile's host, with the host name resolved there (no DNS lookups on your machine). Only the CONNECT request is supported, with no authentication; UDP and BIND are refused, and any other request gets the matching SOCKS5 error reply. |

The listen address defaults to `127.0.0.1`, so only programs on your machine can use a local forward or SOCKS proxy. Typing another address (for example `0.0.0.0`) is allowed; the profile editor then warns `Listens on 0.0.0.0: other machines can reach this tunnel.`

A remote program can never open, change or close a tunnel. A remote forward accepts only the connections the server sends for the forward you asked for; any other channel the server tries to open is refused.

## Defining tunnels

Open the profile editor, pick an SSH profile and use the **Tunnels** section: **Add tunnel**, then a name (unique within the profile), the kind, the addresses and ports, and when it starts. Changes are saved with the profile. The same fields are written to `profiles.toml` (see below).

## Start modes

| Mode | Behavior |
| --- | --- |
| **On demand** (default) | Starts only when you switch it on, in the Tunnels window or from the palette, and runs until you switch it off. With no shell of the profile open it opens a connection of its own, with the same host key and password prompts a shell would show. |
| **With the shell** | Starts when the profile's first shell (tab or pane) connects and stops when the last shell of that profile closes or disconnects. While it waits for a shell it shows `Starts with the shell`. When a disconnected shell is reconnected, the tunnel starts again. |

Details for **With the shell** tunnels:

- Switching one on by hand while no shell of its profile is open makes that run behave like an on-demand one: it stays up until you switch it off. The mode applies again the next time a shell opens.
- Switching one off by hand keeps it off until the next time a first shell connects.
- Nothing is started when the app launches, except **With the shell** tunnels together with their restored shells. A tunnel that was up when you quit is not restarted on its own.

The profile's connection is shared by its shells, the file panel and its tunnels, and it closes only when nothing uses it any more: no shell, no running tunnel.

## The Tunnels window

Open it from the palette (**Tunnels**), from the tunnels indicator in the tab bar, which shows how many tunnels are up, or bind a shortcut to the `tunnels` action in **Keyboard** in Settings (it has none by default, see [Keyboard shortcuts](keyboard-shortcuts#the-keys-table)).

The window lists tunnels grouped by profile. Each row shows the name, the addresses, the state and a switch. The address line reads `localhost:5432 → db.internal:5432 via bastion` for a local forward, `remote 127.0.0.1:9000 → localhost:3000` for a remote forward and `SOCKS 127.0.0.1:1080` for a proxy, with `via` and the jump hosts joined by arrows when the profile has jumps. The search box at the top filters by tunnel and profile name as you type.

| State | Meaning |
| --- | --- |
| `Off` | Not running. |
| `Starts with the shell` | A **With the shell** tunnel waiting for a shell of its profile. |
| `Starting` | Connecting or binding. |
| `Up` | Listening. `Up · 3 connections` (or `1 connection`) while connections are open. |
| `Reconnecting (attempt 2 of 10)` | The connection dropped and the app is trying again; see below. |
| `Failed: <reason>` | Shown in red, with **Retry**. |

A running tunnel keeps the definition it started with. If you edit the tunnel or its profile (host, jump hosts) meanwhile, the row shows `Restart to apply`: switch it off and on again. Deleting a running tunnel or its profile stops it.

In the palette every tunnel is a row `Tunnel: <profile> / <name>` with its state on the right. Choosing it switches the tunnel on or off.

## Reconnecting

When the connection under a running tunnel drops (an on-demand tunnel, or a **With the shell** one you switched on by hand with no shell open), the app tries again by itself and shows `Reconnecting`. The waits are 1, 2, 4, 8 and 16 seconds, then 30 seconds between further attempts, up to the **Reconnect attempts** setting (`tunnel_max_retries`, default `10`, `0` to `100`; `0` turns automatic reconnecting off). Change it in Settings under **Tunnels** or in [`settings.toml`](files#settingstoml).

An automatic attempt never asks you anything. If reconnecting needs you (a changed host key, a password that is not in the keychain, a key passphrase), the tunnel fails at once with `needs your input`; **Retry** then asks as a shell would.

When the attempts run out, or a tunnel cannot start (the port is in use, the server refuses the forward), the tunnel turns red as `Failed: <reason>` and a banner in the focused window says `Tunnel "db" (db-host) failed: <reason>`.

## In profiles.toml

Tunnels are `[[profiles.tunnels]]` tables placed right after their SSH profile. The example on [Profiles and jump hosts](profiles#a-complete-example) has one (`db`, last table).

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `name` | string | required | Name, unique within the profile. |
| `kind` | `"local"`, `"remote"` or `"socks"` | required | What the tunnel does. |
| `bind` | string | `"127.0.0.1"` | Address to listen on: on your machine for `local` and `socks`, on the host for `remote`. |
| `port` | integer, `1` to `65535` | required | Port to listen on. |
| `host` | string | required for `local` and `remote` | Target host. Seen from the profile's host for `local`, from your machine for `remote`. |
| `host_port` | integer, `1` to `65535` | required for `local` and `remote` | Target port. |
| `start` | `"on_demand"` or `"with_shell"` | `"on_demand"` | When it starts. The default is not written to the file. |

```toml
[[profiles.tunnels]]
name = "proxy"
kind = "socks"
port = 1080
```
