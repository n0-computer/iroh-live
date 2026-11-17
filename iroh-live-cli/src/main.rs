//! `irl`: publish and watch live media over iroh.

use clap::{Parser, Subcommand};

mod args;
mod backend;
mod call;
mod devices;
mod import;
mod keys;
mod playback;
#[cfg(all(target_os = "linux", feature = "pipewire"))]
mod portal_grant;
mod publish;
mod rcan;
mod record;
mod rendition;
mod room;
mod run;
mod scan;
mod shutdown;
mod source;
mod source_spec;
mod transport;
mod ui;
mod watch;

/// Publish and watch live audio and video over iroh.
#[derive(Parser, Debug)]
#[command(name = "irl", about, version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The `irl` subcommands.
#[derive(Subcommand, Debug)]
enum Command {
    /// Place or answer a 1:1 video call.
    Call(Box<args::CallArgs>),
    /// List cameras, displays, and audio devices.
    Devices,
    /// Print the public key of a stored key, creating the key on first use.
    Key(args::KeyArgs),
    /// Publish a capture device or a media file.
    Publish(Box<args::PublishArgs>),
    /// Sign or show rcan capability chains, the tokens a relay's principals issue.
    #[command(subcommand)]
    Rcan(args::RcanCommand),
    /// Record a remote broadcast to a file, without a window.
    Record(Box<args::RecordArgs>),
    /// Join a room: publish, and watch everyone else.
    Room(Box<args::RoomArgs>),
    /// Run a multi-stream session described by a TOML file.
    Run(args::RunArgs),
    /// Subscribe to a remote broadcast and play it.
    #[command(visible_alias = "play")]
    Watch(Box<args::WatchArgs>),
}

/// Parses the command line and runs the chosen command.
fn main() -> n0_error::Result {
    // Logs go to stderr: stdout carries what scripts read, such as the key
    // `irl key` prints and the token `irl rcan sign` prints.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();

    // Windowed commands set up inside `block_on`, then hand the main thread to
    // eframe with the runtime entered. `block_on` in an egui callback panics.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let result = match cli.command {
        Command::Call(args) => call::run(*args, &rt),
        Command::Devices => devices::run(&rt),
        Command::Key(args) => keys::run(&args),
        Command::Publish(args) => publish::run(*args, &rt),
        Command::Rcan(command) => rcan::run(&command),
        Command::Record(args) => record::run(*args, &rt),
        Command::Room(args) => room::run(*args, &rt),
        Command::Run(args) => run::run(args, &rt),
        Command::Watch(args) => watch::run(*args, &rt),
    };
    // Waits for the runtime's blocking tasks. After a Ctrl+C, the shutdown
    // watchdog bounds the wait.
    shutdown::step("stopping the runtime");
    drop(rt);
    result
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::{Cli, Command};

    /// Returns an endpoint id and broadcast name to use instead of a ticket.
    fn endpoint_and_name() -> (String, String) {
        let id = iroh::SecretKey::generate().public();
        (id.to_string(), "hello".to_string())
    }

    #[test]
    fn the_command_tree_is_valid() {
        // clap validates `#[arg]` attributes only when it builds the command.
        // Without this test a mistake panics at the user.
        Cli::command().debug_assert();
    }

    #[test]
    fn watch_and_record_name_a_broadcast_the_same_way() {
        let (id, name) = endpoint_and_name();

        let cli = Cli::try_parse_from(["irl", "watch", "--endpoint-id", &id, "--name", &name])
            .expect("the pair is one of the two accepted forms");
        let Command::Watch(args) = cli.command else {
            panic!("expected watch");
        };
        assert_eq!(
            args.remote.ticket().expect("resolves").path().as_str(),
            format!("live/{id}/{name}")
        );

        let cli = Cli::try_parse_from(["irl", "record", "--endpoint-id", &id, "--name", &name])
            .expect("record accepts exactly what watch does");
        let Command::Record(args) = cli.command else {
            panic!("expected record");
        };
        assert_eq!(
            args.remote.ticket().expect("resolves").path().as_str(),
            format!("live/{id}/{name}")
        );
    }

    #[test]
    fn a_broadcast_named_no_way_at_all_is_rejected() {
        let cli = Cli::try_parse_from(["irl", "watch"]).expect("clap accepts no arguments");
        let Command::Watch(args) = cli.command else {
            panic!("expected watch");
        };
        let err = args.remote.ticket().expect_err("nothing names a broadcast");
        assert!(
            err.to_string().contains("--endpoint-id"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn a_relay_token_file_needs_a_relay() {
        Cli::try_parse_from(["irl", "publish", "--relay-token-file", "token.jwt"])
            .expect_err("a token file without --relay");
        let relay = iroh::SecretKey::generate().public().to_string();
        let cli = Cli::try_parse_from([
            "irl",
            "publish",
            "--relay",
            &relay,
            "--relay-token-file",
            "token.jwt",
            "--key",
            "camera",
        ])
        .expect("a relay, a token file and a key");
        let Command::Publish(args) = cli.command else {
            panic!("expected publish");
        };
        assert_eq!(args.transport.key.as_deref(), Some("camera"));
        Cli::try_parse_from([
            "irl",
            "publish",
            "--relay",
            &relay,
            "--relay-token-file",
            "token.jwt",
            "--relay-token",
            "inline",
        ])
        .expect_err("two token sources");
    }

    /// `IRL_RELAY_TOKEN` yields to either flag, and counts only with `--relay`.
    #[test]
    fn the_relay_token_variable_yields_to_the_flags() {
        let token = |args: &[&str], var: &str| {
            let cli = Cli::try_parse_from(args).expect("clap accepts the flags");
            let Command::Publish(args) = cli.command else {
                panic!("expected publish");
            };
            args.transport
                .relay_token_from(Some(var.to_owned()))
                .expect("a valid token")
                .map(|token| token.as_str().to_owned())
        };
        let relay = iroh::SecretKey::generate().public().to_string();
        let file = std::env::temp_dir().join(format!("irl-token-{relay}.jwt"));
        std::fs::write(&file, "from-file\n").expect("writes the token");
        let file_arg = file.to_str().expect("a UTF-8 temp path");

        let with_file = [
            "irl",
            "publish",
            "--relay",
            &relay,
            "--relay-token-file",
            file_arg,
        ];
        assert_eq!(token(&with_file, "from-var").as_deref(), Some("from-file"));
        let inline = [
            "irl",
            "publish",
            "--relay",
            &relay,
            "--relay-token",
            "inline",
        ];
        assert_eq!(token(&inline, "from-var").as_deref(), Some("inline"));
        let var_only = ["irl", "publish", "--relay", &relay];
        assert_eq!(token(&var_only, "from-var").as_deref(), Some("from-var"));
        assert_eq!(token(&["irl", "publish"], "from-var"), None);
        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn the_relay_token_is_redacted_in_debug() {
        let cli = Cli::try_parse_from(["irl", "publish", "--relay-token", "secret-jwt"])
            .expect("clap accepts the flag");
        let Command::Publish(args) = cli.command else {
            panic!("expected publish");
        };
        assert!(!format!("{:?}", args.transport).contains("secret-jwt"));
    }

    #[test]
    fn the_endpoint_id_needs_a_name() {
        let (id, _) = endpoint_and_name();
        Cli::try_parse_from(["irl", "watch", "--endpoint-id", &id])
            .expect_err("--endpoint-id alone does not name a broadcast");
    }

    /// `--scan` names the broadcast, so no ticket is needed.
    #[test]
    fn scanning_stands_in_for_a_ticket() {
        let cli = Cli::try_parse_from(["irl", "watch", "--scan"])
            .expect("--scan supplies the ticket the camera is about to read");
        let Command::Watch(args) = cli.command else {
            panic!("expected watch");
        };
        assert!(args.scan);
        assert!(args.remote.ticket().is_err(), "nothing named a broadcast");
    }

    /// The scan screen is a window, and `--no-video` opens none.
    #[test]
    fn scanning_and_no_video_are_rejected_together() {
        Cli::try_parse_from(["irl", "watch", "--scan", "--no-video"])
            .expect_err("a scan needs a window to draw the camera in");
    }
}
