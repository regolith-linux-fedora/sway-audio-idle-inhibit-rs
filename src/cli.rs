//! Command-line parsing.
//!
//! The dry-print flags, the ignore list and the handling of unknown
//! arguments follow the original C++ program: arguments are read left to
//! right, an unknown argument (or `--ignore-source-outputs` without a value)
//! prints the help and exits successfully, and the dry-print modes take
//! precedence in the order both, both-waybar, sink, source.

use std::ffi::OsString;
use std::time::Duration;

/// At most this many names are kept from `--ignore-source-outputs`.
pub const MAX_IGNORED_SOURCE_OUTPUTS: usize = 100;

/// What the program does with the stream state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Inhibit idle through the session manager while audio is active.
    Idle,
    /// Print `RUNNING` / `NOT RUNNING` for playback or recording.
    DryBoth,
    /// Print a waybar JSON line for playback and recording.
    DryBothWaybar,
    /// Print `RUNNING` / `NOT RUNNING` for playback only.
    DrySink,
    /// Print `RUNNING` / `NOT RUNNING` for recording only.
    DrySource,
}

/// Which kinds of streams can count as activity in idle mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kinds {
    Both,
    SinkOnly,
    SourceOnly,
}

/// The parsed options for a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub mode: Mode,
    pub ignored_source_outputs: Vec<String>,
    pub delay: Duration,
    pub kinds: Kinds,
}

/// The result of parsing the command line.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Run(Options),
    Help,
    Usage(String),
}

/// Parses the arguments after the program name.
pub fn parse(args: &[OsString]) -> Command {
    let mut print_both = false;
    let mut print_both_waybar = false;
    let mut print_sink = false;
    let mut print_source = false;
    let mut sink_only = false;
    let mut source_only = false;
    let mut delay = Duration::ZERO;
    let mut ignored = Vec::new();

    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_string_lossy();
        match arg.as_ref() {
            "-h" | "--help" => return Command::Help,
            "--dry-print-source" => print_source = true,
            "--dry-print-sink" => print_sink = true,
            "--dry-print-both" => print_both = true,
            "--dry-print-both-waybar" => print_both_waybar = true,
            "--sink-only" => sink_only = true,
            "--source-only" => source_only = true,
            "--ignore-source-outputs" if index + 1 < args.len() => {
                index += 1;
                let names = args[index].to_string_lossy();
                for name in names.split(' ').filter(|name| !name.is_empty()) {
                    if ignored.len() >= MAX_IGNORED_SOURCE_OUTPUTS {
                        break;
                    }
                    ignored.push(name.to_owned());
                }
            }
            "--delay" => {
                let Some(value) = args.get(index + 1) else {
                    return Command::Usage("--delay needs a value in seconds".into());
                };
                index += 1;
                match parse_seconds(&value.to_string_lossy()) {
                    Some(seconds) => delay = Duration::from_secs(seconds),
                    None => {
                        return Command::Usage(format!(
                            "invalid --delay value {:?}: expected whole seconds (0-{})",
                            value.to_string_lossy(),
                            u64::MAX
                        ))
                    }
                }
            }
            _ => return Command::Help,
        }
        index += 1;
    }

    if sink_only && source_only {
        return Command::Usage("--sink-only and --source-only cannot be used together".into());
    }

    let mode = if print_both {
        Mode::DryBoth
    } else if print_both_waybar {
        Mode::DryBothWaybar
    } else if print_sink {
        Mode::DrySink
    } else if print_source {
        Mode::DrySource
    } else {
        Mode::Idle
    };
    let kinds = if sink_only {
        Kinds::SinkOnly
    } else if source_only {
        Kinds::SourceOnly
    } else {
        Kinds::Both
    };

    Command::Run(Options {
        mode,
        ignored_source_outputs: ignored,
        delay,
        kinds,
    })
}

/// Parses one or more ASCII digits into seconds, rejecting anything else
/// (signs, whitespace, fractions, exponents) and values above `u64::MAX`.
fn parse_seconds(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The help text, naming the program as it was invoked.
pub fn help(program: &str) -> String {
    let lines = [
        "Usage:".to_owned(),
        format!("\t{program} <OPTION>"),
        "Options:".to_owned(),
        format!("\t {program}\t Inhibits idle if either any sink or any source is running"),
        "\t -h, --help \t\t\t Show help options".to_owned(),
        "\t --dry-print-both \t\t Don't inhibit idle and print if either any sink or any source is running".to_owned(),
        "\t --dry-print-both-waybar \t Same as --dry-print-both but outputs in a waybar friendly manner".to_owned(),
        "\t --dry-print-sink \t\t Don't inhibit idle and print if any sink is running".to_owned(),
        "\t --dry-print-source \t\t Don't inhibit idle and print if any source is running".to_owned(),
        "\t --ignore-source-outputs NAMES \t Don't inhibit idle for these source outputs (space-separated application names)".to_owned(),
        "\t --delay SECONDS \t\t Inhibit idle only once audio has been active for SECONDS without a break (default 0)".to_owned(),
        "\t --sink-only \t\t\t Inhibit idle only for playback (sinks)".to_owned(),
        "\t --source-only \t\t\t Inhibit idle only for recording (sources)".to_owned(),
    ];
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Command {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        parse(&args)
    }

    fn options(args: &[&str]) -> Options {
        match run(args) {
            Command::Run(options) => options,
            other => panic!("expected a run for {args:?}, got {other:?}"),
        }
    }

    #[test]
    fn defaults_to_idle_mode() {
        let parsed = options(&[]);
        assert_eq!(parsed.mode, Mode::Idle);
        assert_eq!(parsed.delay, Duration::ZERO);
        assert_eq!(parsed.kinds, Kinds::Both);
        assert!(parsed.ignored_source_outputs.is_empty());
    }

    #[test]
    fn dry_print_precedence_ignores_order() {
        let all = [
            "--dry-print-source",
            "--dry-print-sink",
            "--dry-print-both-waybar",
            "--dry-print-both",
        ];
        assert_eq!(options(&all).mode, Mode::DryBoth);
        assert_eq!(
            options(&["--dry-print-sink", "--dry-print-both-waybar"]).mode,
            Mode::DryBothWaybar
        );
        assert_eq!(
            options(&["--dry-print-source", "--dry-print-sink"]).mode,
            Mode::DrySink
        );
        assert_eq!(options(&["--dry-print-source"]).mode, Mode::DrySource);
    }

    #[test]
    fn unknown_arguments_and_missing_ignore_value_print_help() {
        assert_eq!(run(&["--bogus"]), Command::Help);
        assert_eq!(run(&["--dry-print-both", "--bogus"]), Command::Help);
        assert_eq!(run(&["--ignore-source-outputs"]), Command::Help);
        assert_eq!(run(&["-h"]), Command::Help);
        assert_eq!(run(&["--help"]), Command::Help);
    }

    #[test]
    fn ignore_list_splits_on_spaces_and_appends() {
        let parsed = options(&[
            "--ignore-source-outputs",
            "  alpha   beta  ",
            "--ignore-source-outputs",
            "gamma",
        ]);
        assert_eq!(parsed.ignored_source_outputs, ["alpha", "beta", "gamma"]);
        assert!(options(&["--ignore-source-outputs", ""])
            .ignored_source_outputs
            .is_empty());
    }

    #[test]
    fn ignore_list_is_capped() {
        let names: Vec<String> = (0..150).map(|n| format!("n{n}")).collect();
        let joined = names.join(" ");
        let parsed = options(&["--ignore-source-outputs", &joined]);
        assert_eq!(
            parsed.ignored_source_outputs.len(),
            MAX_IGNORED_SOURCE_OUTPUTS
        );
    }

    #[test]
    fn delay_accepts_plain_decimal_seconds() {
        assert_eq!(options(&["--delay", "0"]).delay, Duration::ZERO);
        assert_eq!(options(&["--delay", "007"]).delay, Duration::from_secs(7));
        assert_eq!(
            options(&["--delay", "18446744073709551615"]).delay,
            Duration::from_secs(u64::MAX)
        );
    }

    #[test]
    fn invalid_delay_is_a_usage_error() {
        for value in [
            "-1",
            "+1",
            "abc",
            "1.5",
            "",
            " 1",
            "2x",
            "0.0",
            "1e2",
            "NaN",
            "--sink-only",
            "18446744073709551616",
        ] {
            assert!(
                matches!(run(&["--delay", value]), Command::Usage(_)),
                "{value:?}"
            );
        }
        assert!(matches!(run(&["--delay"]), Command::Usage(_)));
    }

    #[test]
    fn sink_only_and_source_only_conflict() {
        assert!(matches!(
            run(&["--sink-only", "--source-only"]),
            Command::Usage(_)
        ));
        assert!(matches!(
            run(&["--source-only", "--sink-only"]),
            Command::Usage(_)
        ));
        assert_eq!(options(&["--sink-only"]).kinds, Kinds::SinkOnly);
        assert_eq!(options(&["--source-only"]).kinds, Kinds::SourceOnly);
    }

    #[test]
    fn help_names_every_option() {
        let text = help("sway-audio-idle-inhibit");
        assert!(text.starts_with("Usage:\n"));
        for option in [
            "--help",
            "-h",
            "--dry-print-both",
            "--dry-print-both-waybar",
            "--dry-print-sink",
            "--dry-print-source",
            "--ignore-source-outputs",
            "--delay",
            "--sink-only",
            "--source-only",
        ] {
            assert!(text.contains(option), "{option}");
        }
    }
}
