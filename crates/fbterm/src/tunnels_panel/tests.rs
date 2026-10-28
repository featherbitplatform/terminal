use fbterm_config::{ProfileId, TunnelKind};
use fbterm_session::ssh::tunnel::TunnelState;

use super::Group;
use crate::app::Message;
use crate::tunnels_tests::{World, key, panel_world, tunnels_of};

fn groups(w: &World, query: &str) -> Vec<Group> {
    super::groups(&w.app.profiles, &w.app.tunnels, query, None)
}

fn names(groups: &[crate::tunnels_panel::Group]) -> Vec<(String, Vec<String>)> {
    groups
        .iter()
        .map(|g| {
            (
                g.profile.clone(),
                g.rows.iter().map(|r| r.name.clone()).collect(),
            )
        })
        .collect()
}

#[test]
fn panel_lists_tunnels_grouped_by_profile() {
    use crate::tunnels_panel::Tone;
    let (mut w, _) = panel_world();
    let g = groups(&w, "");
    assert_eq!(
        names(&g),
        [
            ("srv".to_string(), vec!["web".to_string(), "db".to_string()]),
            (
                "warehouse".to_string(),
                vec!["hook".to_string(), "proxy".to_string()]
            ),
        ],
        "profiles without tunnels are left out; definition order within a profile"
    );
    let web = &g[0].rows[0];
    assert_eq!(web.address, "127.0.0.1:18080 \u{2192} db.internal:5432");
    assert_eq!(web.state, "Off");
    assert_eq!(web.tone, Tone::Off);
    assert!(!web.on && !web.retry && !web.restart_to_apply);
    // A with-the-shell tunnel waiting for its shell says so.
    assert_eq!(g[0].rows[1].state, "Starts with the shell");
    assert_eq!(
        g[1].rows[0].address,
        "remote 127.0.0.1:9000 \u{2192} localhost:3000 via bastion"
    );
    assert_eq!(g[1].rows[1].address, "SOCKS 127.0.0.1:1080 via bastion");

    // Every state, its label and tone.
    w.toggle("web");
    let row = |w: &World| groups(w, "")[0].rows[0].clone();
    assert_eq!(
        (row(&w).state.as_str(), row(&w).tone, row(&w).on),
        ("Starting", Tone::Busy, true)
    );
    let web_key = key(&w.srv, "web");
    for (state, label, tone) in [
        (TunnelState::Up { connections: 0 }, "Up", Tone::Up),
        (
            TunnelState::Up { connections: 1 },
            "Up \u{b7} 1 connection",
            Tone::Up,
        ),
        (
            TunnelState::Up { connections: 3 },
            "Up \u{b7} 3 connections",
            Tone::Up,
        ),
        (
            TunnelState::Reconnecting {
                attempt: 2,
                max: 10,
            },
            "Reconnecting (attempt 2 of 10)",
            Tone::Busy,
        ),
    ] {
        let _ = w.app.update(Message::Tunnel(web_key.clone(), state));
        assert_eq!((row(&w).state.as_str(), row(&w).tone), (label, tone));
    }
    // A changed definition marks the running tunnel.
    w.edit(|p| {
        if let TunnelKind::Local { host_port, .. } = &mut tunnels_of(p)[0].kind {
            *host_port = 5433;
        }
    });
    assert!(row(&w).restart_to_apply);
    // While it runs the address is what is actually listening, not the pending edit.
    assert_eq!(row(&w).address, "127.0.0.1:18080 \u{2192} db.internal:5432");
    // A failure: red, switched off, with Retry.
    let _ = w.app.update(Message::Tunnel(
        web_key,
        TunnelState::Failed("port 18080 already in use".into()),
    ));
    let r = row(&w);
    // Off, it shows the definition the next start uses.
    assert_eq!(r.address, "127.0.0.1:18080 \u{2192} db.internal:5433");
    assert_eq!(r.state, "Failed: port 18080 already in use");
    assert_eq!(r.tone, Tone::Failed);
    assert!(!r.on && r.retry && !r.restart_to_apply);

    // A with-the-shell tunnel switched off by hand while its shell is open is plainly off.
    let _ = w.open_shell();
    assert!(w.running("db"));
    w.toggle("db");
    assert_eq!(groups(&w, "")[0].rows[1].state, "Off");
}

#[test]
fn search_filters_by_tunnel_and_profile() {
    let (w, _) = panel_world();
    // A tunnel name: only that row, under its profile.
    assert_eq!(
        names(&groups(&w, "proxy")),
        [("warehouse".to_string(), vec!["proxy".to_string()])]
    );
    // A profile name: every tunnel of that profile.
    assert_eq!(
        names(&groups(&w, "wareh")),
        [(
            "warehouse".to_string(),
            vec!["hook".to_string(), "proxy".to_string()]
        )]
    );
    // Fuzzy, like the palette, and words may span profile and tunnel.
    assert_eq!(
        names(&groups(&w, "srv db")),
        [("srv".to_string(), vec!["db".to_string()])]
    );
    assert_eq!(
        names(&groups(&w, "wb")),
        [("srv".to_string(), vec!["web".to_string()])]
    );
    assert!(groups(&w, "nothing-like-this").is_empty());
    // Addresses are not searched.
    assert!(groups(&w, "1080").is_empty());
    assert_eq!(groups(&w, "  ").len(), 2, "a blank query lists everything");
}

fn highlighted(w: &World, query: &str, profile: Option<ProfileId>) -> Vec<(String, bool)> {
    super::groups(&w.app.profiles, &w.app.tunnels, query, profile)
        .into_iter()
        .map(|g| (g.profile, g.highlighted))
        .collect()
}

fn id_of(w: &World, name: &str) -> ProfileId {
    w.app
        .profiles
        .profiles
        .iter()
        .find(|p| p.name == name)
        .expect("profile")
        .id
}

fn s(a: &str, h: bool) -> (String, bool) {
    (a.to_string(), h)
}

#[test]
fn highlighted_group_comes_first_others_keep_order() {
    let (w, _) = panel_world();
    let wh = id_of(&w, "warehouse");
    assert_eq!(
        highlighted(&w, "", Some(wh)),
        [s("warehouse", true), s("srv", false)]
    );
    let g = super::groups(&w.app.profiles, &w.app.tunnels, "", Some(wh));
    assert_eq!(g[0].profile_id, wh);
    assert_eq!(g[0].rows.len(), 2);
}

#[test]
fn no_highlight_keeps_profile_order() {
    let (w, _) = panel_world();
    assert_eq!(
        highlighted(&w, "", None),
        [s("srv", false), s("warehouse", false)]
    );
    let srv = id_of(&w, "srv");
    assert_eq!(
        highlighted(&w, "", Some(srv)),
        [s("srv", true), s("warehouse", false)]
    );
}

#[test]
fn filtered_out_highlight_highlights_nothing() {
    let (w, _) = panel_world();
    let wh = id_of(&w, "warehouse");
    assert_eq!(highlighted(&w, "web", Some(wh)), [s("srv", false)]);
}

#[test]
fn highlight_of_a_missing_profile_is_ignored() {
    let (w, _) = panel_world();
    assert_eq!(
        highlighted(&w, "", Some(ProfileId::new())),
        [s("srv", false), s("warehouse", false)]
    );
}

#[test]
fn local_profile_highlights_nothing() {
    let (w, _) = panel_world();
    let local = id_of(&w, "local");
    assert_eq!(
        highlighted(&w, "", Some(local)),
        [s("srv", false), s("warehouse", false)]
    );
    let bastion = id_of(&w, "bastion");
    assert_eq!(
        highlighted(&w, "", Some(bastion)),
        [s("srv", false), s("warehouse", false)]
    );
}
