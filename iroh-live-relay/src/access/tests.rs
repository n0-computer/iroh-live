use std::path::PathBuf;

use clap::Parser;
use iroh_live_node::{
    SessionRequest,
    policy::{Event, PeerRequest},
};

use super::*;
use crate::RcanPrincipal;

#[derive(Parser)]
struct Cli {
    #[command(flatten)]
    relay: Config,
}

fn parse(args: &[&str]) -> Config {
    Cli::try_parse_from(std::iter::once("relay").chain(args.iter().copied()))
        .map(|cli| cli.relay)
        .unwrap_or_else(|err| panic!("{args:?} parse: {err}"))
}

/// A key file in the temp dir, removed on drop.
struct KeysFile(PathBuf);

impl KeysFile {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "iroh-live-relay-access-keys-{}.jwk",
            iroh::SecretKey::generate().public()
        ));
        moq_auth::Key::generate(moq_auth::Algorithm::HS256, None)
            .expect("a key")
            .to_file(&path)
            .expect("writes the key");
        Self(path)
    }

    fn arg(&self) -> &str {
        self.0.to_str().expect("a UTF-8 temp path")
    }
}

impl Drop for KeysFile {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}

fn principal() -> String {
    iroh::SecretKey::generate().public().to_string()
}

/// Returns the message of the access error `config` fails with.
fn refused(config: &Config) -> String {
    match config.policies() {
        Err(Error::AccessConfig { message, .. }) => message,
        Err(other) => panic!("not an access error: {other:#}"),
        Ok(policies) => panic!("accepted as {:?}", policies.access),
    }
}

#[test]
fn the_access_mode_is_inferred() {
    assert_eq!(parse(&[]).access_mode().unwrap(), Access::Open);
    let keys = KeysFile::new();
    let key = principal();
    for args in [
        vec!["--auth-key", keys.arg()],
        vec!["--rcan-principal", key.as_str()],
        vec!["--auth-url", "https://auth.example"],
        vec!["--auth-public", "demo/**"],
        vec!["--auth-public-publish", "demo/**"],
        vec!["--auth-public-subscribe", "demo/**"],
        vec!["--auth-public-own-paths"],
        vec!["--auth-own-paths"],
    ] {
        assert_eq!(
            parse(&args).access_mode().unwrap(),
            Access::Private,
            "{args:?}"
        );
    }
    // Neither the session bound nor the pull flag is an auth flag.
    assert_eq!(
        parse(&["--max-session", "1h", "--open-pulls"])
            .access_mode()
            .unwrap(),
        Access::Open
    );
    let mut explicit = parse(&[]);
    explicit.access = Some(Access::Private);
    assert_eq!(explicit.access_mode().unwrap(), Access::Private);
}

#[test]
fn a_contradictory_access_config_is_refused() {
    let keys = KeysFile::new();
    let mut open_with_key = parse(&["--auth-key", keys.arg()]);
    open_with_key.access = Some(Access::Open);
    assert!(refused(&open_with_key).contains("--auth-key"));
    let mut open_with_url = parse(&["--auth-url", "https://auth.example"]);
    open_with_url.access = Some(Access::Open);
    assert!(refused(&open_with_url).contains("Access::Open"));

    let message = refused(&parse(&["--auth-public-own-paths"]));
    assert!(message.contains("publishers only"), "{message}");
    let mut nothing = parse(&[]);
    nothing.access = Some(Access::Private);
    assert!(refused(&nothing).contains("every reader"));
    let message = refused(&parse(&["--auth-public-publish", "demo/**"]));
    assert!(message.contains("every reader"), "{message}");

    for flag in ["--auth-own-paths", "--allow-unexpiring-tokens"] {
        let mut args = vec![flag];
        args.extend(["--auth-public-subscribe", "demo/**"]);
        let message = refused(&parse(&args));
        assert!(message.contains(flag), "{message}");
    }
}

#[test]
fn an_iroh_policy_needs_auth_settings_or_an_explicit_mode() {
    let mut custom = parse(&[]);
    custom.iroh_policy = Some(Arc::new(relay_policy()));
    assert!(refused(&custom).contains("Config::iroh_policy"));
    custom.access = Some(Access::Open);
    let policies = custom.policies().expect("open, stated");
    assert_eq!(policies.access, Access::Open);
    assert!(
        policies
            .summary
            .iter()
            .any(|line| line.contains("custom policy"))
    );
    custom.access = Some(Access::Private);
    let policies = custom
        .policies()
        .expect("private with a custom iroh policy");
    assert!(
        policies.summary.iter().any(|line| line.contains("refused")),
        "{:?}",
        policies.summary
    );
}

#[test]
fn a_public_publish_pattern_cannot_reach_iroh_paths() {
    for pattern in ["live/**", "**", "*/*/**", "rooms/*/*/**", "live/*/*"] {
        for flag in ["--auth-public", "--auth-public-publish"] {
            let message = refused(&parse(&[flag, pattern]));
            assert!(message.contains("--auth-public-subscribe"), "{message}");
        }
    }
    parse(&["--auth-public", "demo/**"])
        .policies()
        .expect("outside live/ and rooms/");
    parse(&["--auth-public-subscribe", "**"])
        .policies()
        .expect("reading is fine");
}

#[test]
fn a_public_pattern_without_a_wildcard_is_refused() {
    for flag in [
        "--auth-public",
        "--auth-public-publish",
        "--auth-public-subscribe",
    ] {
        let message = refused(&parse(&[flag, "demo"]));
        assert!(message.contains("`demo/**`"), "{message}");
    }
}

#[test]
fn a_principal_with_a_wildcard_root_is_refused() {
    let key = principal();
    let arg = format!("{key}@spaces/*");
    assert!(
        Cli::try_parse_from(["relay", "--rcan-principal", &arg]).is_err(),
        "clap parses the flag"
    );
    let mut config = parse(&[]);
    config.rcan_principals = vec![RcanPrincipal::new(key.parse().unwrap()).with_root("spaces/*")];
    assert!(refused(&config).contains("literal path"));
}

#[test]
fn a_zero_max_session_is_refused() {
    let keys = KeysFile::new();
    let config = parse(&["--auth-key", keys.arg(), "--max-session", "0s"]);
    assert!(refused(&config).contains("--max-session"));
}

/// Decides an iroh client with no token under `policies`.
fn anonymous_iroh(policies: &Policies) -> Option<iroh_live_node::Grant> {
    let peer = iroh::SecretKey::generate().public();
    let request = PeerRequest::new(peer, SessionRequest::default(), Event::Connect);
    n0_future::future::block_on(DynPolicy::decide(&*policies.iroh, &request))
        .ok()
        .map(|authorization| authorization.grant)
}

#[test]
fn the_modes_compose_the_iroh_policy() {
    let open = parse(&[]).policies().expect("open");
    let grant = anonymous_iroh(&open).expect("an open relay admits anyone");
    assert!(grant.allows_subscribe("live/someone/cam"));
    assert!(open.pull_tokens.is_none());

    let key = principal();
    let private = parse(&["--rcan-principal", &key])
        .policies()
        .expect("private");
    assert!(anonymous_iroh(&private).is_none());
    assert!(private.pull_tokens.is_some());

    let area = parse(&["--rcan-principal", &key, "--auth-public", "demo/**"])
        .policies()
        .expect("a public area");
    let grant = anonymous_iroh(&area).expect("the public area admits anyone");
    assert!(grant.allows_subscribe("demo/stage") && grant.allows_publish("demo/stage"));
    assert!(!grant.allows_subscribe("live/someone/cam"));
}

#[test]
fn the_startup_line_names_the_mode_and_its_cause() {
    let open = parse(&[]).policies().expect("open");
    assert!(open.summary[0].starts_with("access: open"));
    assert!(
        open.warnings
            .iter()
            .any(|warning| warning.contains("access is open"))
    );

    let keys = KeysFile::new();
    let key = principal();
    let private = parse(&[
        "--auth-key",
        keys.arg(),
        "--rcan-principal",
        &format!("{key}@spaces/team"),
        "--auth-public-subscribe",
        "demo/**",
        "--bind",
        "127.0.0.1:0",
    ])
    .policies()
    .expect("private");
    let text = private.summary.join("\n");
    assert!(
        text.contains(
            "access: private (from --auth-key, --auth-public-subscribe, --rcan-principal)"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "without a token: browsers and iroh clients subscribe demo/**, publish nothing"
        ),
        "{text}"
    );
    assert!(text.contains("at spaces/team"), "{text}");
    assert!(text.contains("for at most 24h"), "{text}");
    assert!(private.warnings.is_empty(), "{:?}", private.warnings);

    let anywhere = parse(&["--auth-key", keys.arg()])
        .policies()
        .expect("private");
    assert!(
        anywhere
            .warnings
            .iter()
            .any(|warning| warning.contains("plain HTTP")),
        "{:?}",
        anywhere.warnings
    );
}

#[test]
fn open_pulls_warn_when_anonymous_sessions_read_pullable_paths() {
    let key = principal();
    let reads_live = parse(&[
        "--rcan-principal",
        &key,
        "--auth-public-subscribe",
        "**",
        "--open-pulls",
    ])
    .policies()
    .expect("private");
    assert!(
        reads_live
            .warnings
            .iter()
            .any(|warning| warning.contains("--open-pulls")),
        "{:?}",
        reads_live.warnings
    );
    // A ticket's name may span segments, so a pattern below `live/*/*` still reaches pulls.
    let deep = parse(&[
        "--rcan-principal",
        &key,
        "--auth-public-subscribe",
        "live/*/cams/hd/**",
        "--open-pulls",
    ])
    .policies()
    .expect("private");
    assert!(
        deep.warnings
            .iter()
            .any(|warning| warning.contains("--open-pulls")),
        "{:?}",
        deep.warnings
    );
    let demo_only = parse(&[
        "--rcan-principal",
        &key,
        "--auth-public-subscribe",
        "demo/**",
        "--open-pulls",
    ])
    .policies()
    .expect("private");
    assert!(
        !demo_only
            .warnings
            .iter()
            .any(|warning| warning.contains("--open-pulls")),
        "{:?}",
        demo_only.warnings
    );
}

#[test]
fn an_auth_url_without_an_iroh_verifier_warns() {
    let policies = parse(&["--auth-url", "https://auth.example"])
        .policies()
        .expect("private");
    assert!(policies.browsers.is_none());
    assert!(
        policies
            .warnings
            .iter()
            .any(|warning| warning.contains("--auth-url decides browsers only"))
    );
}

#[test]
fn the_startup_line_hides_the_auth_servers_secret() {
    let policies = parse(&["--auth-url", "https://relay:hunter2@auth.example/"])
        .policies()
        .expect("private");
    let text = policies.summary.join("\n");
    assert!(text.contains("https://auth.example/"), "{text}");
    assert!(!text.contains("hunter2"), "{text}");
}
