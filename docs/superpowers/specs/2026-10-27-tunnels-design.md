# Tunnels: design

Status: approved by the owner, 2026-10-27. Milestone 3 of
`2026-10-23-featherbit-terminal-design.md` (sections 5.3 "Connection model" and 5.7), which this
document refines. Where they differ, this document wins for tunnels.

## 1. Intent

The owner reaches services that live behind SSH hosts from tools on their own machine: the
typical case is a Postgres database behind a bastion, reached through a jump host, with the
database client pointed at `localhost`. A shell on that host is optional. Tunnels should be
defined once in the SSH profile, switched on in a click (or come up on their own with the
shell), and say clearly when they are not working.

Success: the owner adds a "db" local forward (local 5432 to `db.internal:5432`) to a profile
whose jump chain goes through the bastion, switches it on from the palette without opening a
shell, connects with their database client to `localhost:5432`, and sees the tunnel up with one
active connection in the tunnels panel.

## 2. Goals

1. Tunnels are **defined per SSH profile** and ride that profile's shared connection, therefore
   its jump chain.
2. Kinds: **local forward** (the owner's case), **remote forward** and **dynamic SOCKS proxy**.
3. Two start modes, chosen per tunnel (owner's decision, 2026-10-27):
   - **On demand:** switched on and off by the user from the tunnels panel or the palette; once
     on it keeps running independently of any shell, opening the profile's connection without a
     shell if none is open, until switched off or the app quits.
   - **With the shell:** starts when a shell (tab or pane) of that profile opens, and closes on
     its own when the last shell of that profile closes or disconnects.
4. A **tunnels panel** lists every tunnel of every profile with its state, and switches each on
   or off. The **palette** offers the same toggles by tunnel name.
5. **Failures are visible:** when a tunnel cannot start or drops, a notification appears and the
   tunnel shows red in the panel with the reason (e.g. "port 5432 already in use").

## 3. Non-goals and anti-goals

- No ad-hoc tunnels outside profiles in this version (every tunnel is saved in a profile).
- No traffic inspection, bandwidth graphs or per-connection logs.
- No forwarding of local Unix sockets, X11 or agent forwarding (agent forwarding stays deferred
  with the other auth work).
- Anti-goals, a failure even if tunnels work:
  - A remote program can never open, change or close a tunnel. Tunnels exist only because the
    user saved them in a profile and started them (or opened that profile's shell for a "with
    the shell" tunnel).
  - Nothing listens on a non-loopback address unless the user explicitly asked for it in the
    tunnel; the default bind address is `127.0.0.1`.
  - A failing tunnel never takes down the profile's shells, and a dropped shell never takes down
    an on-demand tunnel.
  - Tunnels never ask for secrets a shell wouldn't: they authenticate exactly like the profile's
    shells (same prompts, same keychain entries), one connection, one prompt.

## 4. Behaviour

**Definition (in the profile).** Each tunnel has: a name (unique within the profile), a kind,
and per kind:
- Local forward: local bind address (default `127.0.0.1`) and port; target host and port as
  seen from the profile's host.
- Remote forward: remote bind address and port on the profile's host; target host and port as
  seen from this machine.
- SOCKS proxy: local bind address (default `127.0.0.1`) and port.
- Start mode: on demand (default) or with the shell.
Tunnels are edited in the profile editor, in a Tunnels section of SSH profiles *(my guess,
approved)*, and stored in `profiles.toml` with the profile.

**States.** Off; starting (connecting or binding); up, with the number of active connections;
reconnecting (with the attempt number); failed, with the reason. "With the shell" tunnels that are waiting for a shell show as off with
a note that they start with the shell.

**Panel.** *(Approved by the owner 2026-10-27, with a search bar.)* A window like Settings, opened from the palette and from a small tunnels indicator in
the tab bar that shows how many tunnels are up *(my guess, approved)*. It lists tunnels grouped
by profile: name, kind, the addresses in plain form (`localhost:5432 → db.internal:5432 via
bastion`), state, and an on/off switch (for "with the shell" tunnels the switch starts or stops
it now, and the mode applies again the next time a shell opens). A failed tunnel offers Retry. A search bar at the top filters the list as you type, matching tunnel names and profile names (fuzzy, like the palette).

**Palette.** Each tunnel appears as an action `Tunnel: <profile> / <name>`, toggling it, with its
state shown on the row.

**Failures and reconnecting** *(owner's decision, 2026-10-27)*.
- When the connection under an up on-demand tunnel drops, fbterm **keeps retrying on its own**
  and shows the tunnel as **reconnecting**, with backoff between attempts (1, 2, 4, 8, 16 s,
  then every 30 s). The maximum number of attempts has a default of **10** and is configurable
  in Settings (`tunnel_max_retries`; 0 turns automatic retrying off).
- When the attempts run out, or the tunnel cannot start at all (port in use, server refusal),
  it turns **failed**: an in-app notification banner in the focused window *(my guess,
  approved)* says which tunnel and why, and the panel shows it red with Retry.
- *(my call)* An automatic attempt never shows a prompt: if reconnecting would need the user
  (a changed host key, a password that isn't in the keychain, a key passphrase), the tunnel
  fails at once with the reason "needs your input" and Retry asks as a shell would.
- "With the shell" tunnels follow their shells: when a disconnected shell is reconnected (Enter),
  its tunnels start again.

**Connections.** Tunnels share the profile's one SSH connection with its shells and file panel
(main design 5.3). The connection closes only when nothing uses it: no shell, no up tunnel, no
file panel. Starting an on-demand tunnel with no shell open opens a connection without a shell,
with the same host-key and password prompts as a shell would show.

**Settled after the builder check** *(my calls, 2026-10-27)*:
- Switching on a "with the shell" tunnel by hand while no shell of its profile is open makes
  that run behave like on demand: it stays up until switched off. The mode applies again the
  next time a shell opens.
- A running tunnel keeps the definition it started with. Edits to the tunnel or its profile
  (host, jump chain) apply the next time it starts; the panel shows "restart to apply" on a
  tunnel whose definition changed, and deleting a running tunnel or its profile stops it.
- The SOCKS proxy is SOCKS5, CONNECT only, no authentication, with hostnames resolved on the
  remote side (no local DNS lookups); no UDP or BIND.

**Settled in the final review** *(my calls, 2026-10-27)*:
- A tunnel started from the Tunnels window asks its questions over the terminal window used
  last and raises it; with no terminal window open, a tunnel that needs to ask fails with
  "Open a terminal window to answer the sign-in question".
- Renaming a running tunnel (nothing else changed) keeps it running under the new name, marked
  "restart to apply". While a tunnel runs, the panel shows the addresses it started with.
- A tunnel whose saved definition fails validation (a hand-edited `profiles.toml`) is never
  started; it shows failed with the validation message.
- A local or SOCKS tunnel binds its port before it connects, so a port in use fails it before
  any prompt. A changed host key during a reconnect counts as "needs your input".
- The shared connection is reused only while the profile's route (its SSH settings and its jump
  chain's) is unchanged; after an edit new shells connect afresh, and running tunnels keep the
  old connection until restarted.

**Relaunch.** Tunnels are part of the profile, so they survive relaunch as definitions.
*(my call)* On-demand tunnels that were up when the app quit are not restarted automatically on
relaunch; "with the shell" tunnels start with their restored shells.

## 5. Constraints

- Everything in the main design and `CLAUDE.md` holds: in-process `russh` only (never the system
  `ssh`), cross-platform, Graphite look, no naming of other terminal applications, secrets only
  in the keychain, Conventional Commits with no attribution.
- Binding a privileged port (below 1024) or a port already in use fails with a clear reason; a
  non-loopback bind address shows a warning in the profile editor.
- Remote forwards need the server to allow them; a refusal is a failure with the server's reason.
- The redraw contract holds: connection counts update the panel and indicator through events,
  never a polling loop.
- Live SSH tests (Docker, as the existing `ssh_live` job) cover each kind through a jump host.

## 6. Left to the builder

- Data model and TOML shape of a tunnel; validation messages; how names are made unique.
- How tunnel events (bound, connection opened/closed, failed) flow from `fbterm-session` to the
  app; how "with the shell" reference counting is tracked.
- Panel layout, indicator look and placement in the tab bar, the profile editor section's
  layout.
- Whether the notification is the existing notice banner or a new transient toast.
