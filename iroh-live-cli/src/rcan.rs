//! `irl rcan`: signs and shows rcan capability chains.
//!
//! A relay with `--rcan-principal <KEY>` admits iroh clients that present a
//! chain the key issued. `irl rcan sign` issues one with the principal's
//! stored key, or delegates one with the key its last link names, and prints
//! the token. The library side is [`Chain`].

use std::{
    io::Read,
    path::Path,
    time::{Duration, SystemTime},
};

use iroh::{EndpointId, PublicKey};
use iroh_live::node::{
    SessionRequest,
    policy::{Chain, Event, MoqCapability, PeerRequest, RcanVerifier},
};
use moq_net::{Pattern, Patterns};
use n0_error::{Result, anyerr};
use rcan::Expires;

use crate::{
    args::{RcanCommand, RcanShowArgs, RcanSignArgs},
    keys,
};

/// Runs an `irl rcan` subcommand.
pub(crate) fn run(command: &RcanCommand) -> Result {
    match command {
        RcanCommand::Sign(args) => println!("{}", sign(args)?),
        RcanCommand::Show(args) => print!("{}", show(args)?),
    }
    Ok(())
}

/// Signs the chain `args` describe with their stored key and returns its token.
fn sign(args: &RcanSignArgs) -> Result<String> {
    sign_with(&keys::stored_secret_key(&args.key)?, args)
}

/// Signs the chain `args` describe with `key` and returns its token.
fn sign_with(key: &iroh::SecretKey, args: &RcanSignArgs) -> Result<String> {
    let expires = Expires::valid_for(args.ttl);
    let narrowed = args.root.is_some() || !args.subscribe.is_empty() || !args.publish.is_empty();
    let chain = match &args.chain {
        None => {
            if args.subscribe.is_empty() && args.publish.is_empty() {
                return Err(anyerr!(
                    "a new chain grants nothing without --subscribe or --publish"
                ));
            }
            let capability = capability(args.root.as_deref().unwrap_or(""), args);
            Chain::issue(
                key.as_signing_key(),
                args.to.as_verifying_key(),
                capability,
                expires,
            )
        }
        Some(path) => {
            let chain = read_chain(path)?;
            let narrower = narrowed.then(|| {
                let root = args
                    .root
                    .clone()
                    .unwrap_or_else(|| chain.capability().root.clone());
                capability(&root, args)
            });
            chain
                .delegate(
                    key.as_signing_key(),
                    args.to.as_verifying_key(),
                    narrower,
                    expires,
                )
                .map_err(|rejection| anyerr!("cannot delegate the chain: {}", rejection.reason()))?
        }
    };
    Ok(chain.to_token())
}

/// Returns the capability under `root` with the patterns `args` name.
fn capability(root: &str, args: &RcanSignArgs) -> MoqCapability {
    MoqCapability::new(root.trim_matches('/'))
        .with_subscribe(args.subscribe.iter().cloned().collect::<Patterns>())
        .with_publish(args.publish.iter().cloned().collect::<Patterns>())
}

/// Returns the description of the chain `args` names, and of the grant a relay computes.
fn show(args: &RcanShowArgs) -> Result<String> {
    let chain = read_chain(&args.chain)?;
    let mut out = String::new();
    for (index, link) in chain.links().iter().enumerate() {
        let issuer =
            PublicKey::from_bytes(link.issuer().as_bytes()).map_err(|err| anyerr!("{err}"))?;
        let audience =
            PublicKey::from_bytes(link.audience().as_bytes()).map_err(|err| anyerr!("{err}"))?;
        let capability = link.capability();
        out.push_str(&format!("link {}: {issuer} -> {audience}\n", index + 1));
        out.push_str(&format!(
            "  root {:?}: subscribe {}, publish {}\n",
            capability.root,
            patterns(&capability.subscribe),
            patterns(&capability.publish)
        ));
        let expires = match link.expires() {
            Expires::Never => "never".to_owned(),
            Expires::At(secs) => expiry(SystemTime::UNIX_EPOCH + Duration::from_secs(*secs)),
        };
        out.push_str(&format!("  expires {expires}\n"));
    }
    let Some(principal) = &args.principal else {
        return Ok(out);
    };
    let (key, root) = principal.split_once('@').unwrap_or((principal, ""));
    let key: PublicKey = key.parse().map_err(|err| {
        anyerr!("--principal {principal:?}: the key is not an endpoint id: {err}")
    })?;
    let peer: EndpointId = match args.peer {
        Some(peer) => peer,
        None => PublicKey::from_bytes(chain.holder().as_bytes()).map_err(|err| anyerr!("{err}"))?,
    };
    let verifier = RcanVerifier::new(key.as_verifying_key(), root.trim_matches('/'));
    let request = PeerRequest::new(peer, SessionRequest::default(), Event::Connect);
    match verifier.verify_chain(&chain, &request) {
        Ok(authorization) => {
            out.push_str(&format!(
                "a relay with --rcan-principal {principal} admits {}:\n  subscribe {}, publish {}\n  until {}\n",
                peer.fmt_short(),
                patterns(&authorization.grant.subscribe),
                patterns(&authorization.grant.publish),
                authorization.expires.map_or_else(|| "never".to_owned(), expiry),
            ));
        }
        Err(rejection) => {
            out.push_str(&format!(
                "a relay with --rcan-principal {principal} refuses {}: {}\n",
                peer.fmt_short(),
                rejection
            ));
        }
    }
    Ok(out)
}

/// Reads a chain from `path`, or from standard input for `-`.
fn read_chain(path: &Path) -> Result<Chain> {
    let mut text = String::new();
    if path == Path::new("-") {
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|err| anyerr!("failed to read the chain from standard input: {err}"))?;
    } else {
        text = std::fs::read_to_string(path)
            .map_err(|err| anyerr!("failed to read {}: {err}", path.display()))?;
    }
    text.trim()
        .parse()
        .map_err(|rejection: iroh_live::node::policy::Rejection| {
            anyerr!(
                "{} holds no capability chain: {}",
                path.display(),
                rejection.reason()
            )
        })
}

/// Formats `patterns` as a comma-separated list, or "nothing".
fn patterns(patterns: &Patterns) -> String {
    if patterns.is_empty() {
        return "nothing".to_owned();
    }
    patterns
        .iter()
        .map(Pattern::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Formats an expiry as a UTC time, and how far from now it is.
fn expiry(at: SystemTime) -> String {
    let when = humantime::format_rfc3339_seconds(at);
    match at.duration_since(SystemTime::now()) {
        Ok(left) => format!(
            "{when} (in {})",
            humantime::format_duration(Duration::from_secs(left.as_secs()))
        ),
        Err(_) => format!("{when} (expired)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes `token` to a file in `dir` and returns its path.
    fn chain_file(dir: &Path, name: &str, token: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, token).expect("writes the chain");
        path
    }

    fn sign_args(to: PublicKey) -> RcanSignArgs {
        RcanSignArgs {
            key: "unused".to_owned(),
            to,
            chain: None,
            root: None,
            subscribe: Vec::new(),
            publish: Vec::new(),
            ttl: Duration::from_secs(3600),
        }
    }

    /// Issues a membership, delegates it to a device, and checks what a relay grants the device.
    #[test]
    fn sign_issues_and_delegates_a_chain_a_relay_admits() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let (team, alice, device) = (
            iroh::SecretKey::generate(),
            iroh::SecretKey::generate(),
            iroh::SecretKey::generate(),
        );

        let mut issue = sign_args(alice.public());
        issue.subscribe = vec![Pattern::all()];
        let issued = sign_with(&team, &issue).expect("issued");
        let issued_path = chain_file(dir.path(), "alice.rcan", &issued);

        let mut delegate = sign_args(device.public());
        delegate.chain = Some(issued_path.clone());
        delegate.ttl = Duration::from_secs(600);
        let delegated: Chain = sign_with(&alice, &delegate)
            .expect("delegated")
            .parse()
            .expect("a chain");
        assert_eq!(delegated.links().len(), 2);

        let verifier = RcanVerifier::new(team.public().as_verifying_key(), "");
        let request = PeerRequest::new(device.public(), SessionRequest::default(), Event::Connect);
        let authorization = verifier
            .verify_chain(&delegated, &request)
            .expect("admitted");
        assert!(authorization.grant.allows_subscribe("live/someone/cam"));
        assert!(authorization.grant.publish.is_empty());
        let expires = authorization.expires.expect("an expiry");
        assert!(expires <= SystemTime::now() + Duration::from_secs(601));

        // Narrowing to something the chain does not cover is refused.
        let mut widen = sign_args(device.public());
        widen.chain = Some(issued_path.clone());
        widen.publish = vec!["live/**".parse().expect("a pattern")];
        let err = sign_with(&alice, &widen).expect_err("wider");
        assert!(err.to_string().contains("wider"), "{err}");

        // Another key cannot delegate alice's chain.
        let mut foreign = sign_args(device.public());
        foreign.chain = Some(issued_path);
        sign_with(&team, &foreign).expect_err("not the holder");

        let shown = show(&RcanShowArgs {
            chain: chain_file(dir.path(), "device.rcan", &delegated.to_token()),
            principal: Some(team.public().to_string()),
            peer: None,
        })
        .expect("shown");
        assert!(shown.contains("link 2"), "{shown}");
        assert!(shown.contains("admits"), "{shown}");

        let mut empty = sign_args(alice.public());
        empty.root = Some("spaces".to_owned());
        sign_with(&team, &empty).expect_err("grants nothing");
    }
}
