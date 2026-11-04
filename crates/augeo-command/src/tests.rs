use std::collections::BTreeMap;

use super::*;

fn apply(input: &str) -> Target {
    match parse(input) {
        Ok(Command::Apply(target)) => target,
        other => panic!("{input}: expected Apply, got {other:?}"),
    }
}

fn error(input: &str) -> String {
    match parse(input) {
        Err(e) => e.message,
        Ok(c) => panic!("{input}: expected an error, got {c:?}"),
    }
}

#[test]
fn required_forms_select_the_right_numbers() {
    for (input, expected) in [
        ("auge!cpu#1", vec![1]),
        ("auge!cpu#1-3", vec![1, 2, 3]),
        ("auge!cpu#1,3", vec![1, 3]),
        ("auge!cpu#1-3,5", vec![1, 2, 3, 5]),
        ("auge!cpu#5,1-3", vec![1, 2, 3, 5]),
        ("auge!cpu#2,2,1-2", vec![1, 2]),
        ("auge!cpu#3-3", vec![3]),
    ] {
        let target = apply(input);
        assert_eq!(target.component, Component::Cpu, "{input}");
        assert_eq!(target.numbers(), expected, "{input}");
    }
}

#[test]
fn whitespace_and_case_are_ignored() {
    for input in [
        "  auge!cpu#1-3,5  ",
        "AUGE!CPU#1-3,5",
        "auge! cpu #1 - 3 , 5",
        "auge!cpu\t#1-3,\t5",
    ] {
        assert_eq!(apply(input).numbers(), vec![1, 2, 3, 5], "{input}");
    }
}

#[test]
fn every_component_opens() {
    for component in Component::ALL {
        let input = format!("auge!{component}");
        assert_eq!(parse(&input), Ok(Command::Open(component)));
        assert_eq!(Component::from_name(component.as_str()), Some(component));
    }
}

#[test]
fn simple_commands() {
    for (input, expected) in [
        ("auge!quit", Command::Quit),
        ("auge!check", Command::Check),
        ("auge!myths", Command::Myths),
        ("auge!log", Command::Log),
        ("auge!undo-all", Command::UndoAll),
        ("auge!timer", Command::Timer),
        ("auge!polling", Command::Polling),
        ("auge!restore-point", Command::RestorePoint),
        ("auge!elevate", Command::Elevate),
        ("auge!reboot", Command::Reboot),
        ("auge!help", Command::Help(None)),
        ("auge!help bench", Command::Help(Some("bench".into()))),
        ("auge!status", Command::Status(None)),
        ("auge!status gpu", Command::Status(Some(Component::Gpu))),
        (
            "auge!preset Recommended",
            Command::Preset("recommended".into()),
        ),
    ] {
        assert_eq!(parse(input), Ok(expected), "{input}");
    }
}

#[test]
fn targeted_commands() {
    assert!(matches!(parse("auge!undo cpu#1"), Ok(Command::Undo(t)) if t.numbers() == [1]));
    assert!(matches!(parse("auge!info gpu#2"), Ok(Command::Info(t)) if t.numbers() == [2]));
    assert!(
        matches!(parse("auge!dry cpu#1-2"), Ok(Command::Dry(DryTarget::Tweaks(t))) if t.numbers() == [1, 2])
    );
    assert_eq!(
        parse("auge!dry recommended"),
        Ok(Command::Dry(DryTarget::Preset("recommended".into())))
    );
}

#[test]
fn bench_commands() {
    for (input, expected) in [
        ("auge!bench", BenchArgs::Open),
        ("auge!bench start", BenchArgs::Start),
        ("auge!bench list", BenchArgs::List),
        ("auge!bench aa", BenchArgs::NoiseFloor),
        (
            "auge!bench compare 12 run_13",
            BenchArgs::Compare("12".into(), "run_13".into()),
        ),
    ] {
        assert_eq!(parse(input), Ok(Command::Bench(expected)), "{input}");
    }
    assert!(
        matches!(parse("auge!bench ab gpu#3"), Ok(Command::Bench(BenchArgs::AbTest(t))) if t.numbers() == [3])
    );
}

#[test]
fn game_forms_keep_the_exe_case() {
    let target = apply("auge!game#1,3@CS2.exe");
    assert_eq!(target.exe.as_deref(), Some("CS2.exe"));
    assert_eq!(target.numbers(), vec![1, 3]);
    assert_eq!(
        parse("auge!game My Game-2.exe"),
        Ok(Command::SelectGame("My Game-2.exe".into()))
    );
}

#[test]
fn error_messages() {
    for (input, expected) in [
        (
            "auge!cpuu#1",
            "unknown component 'cpuu'. Did you mean 'cpu'?",
        ),
        (
            "auge!cpu#",
            "expected a tweak number after '#'. Example: auge!cpu#1",
        ),
        ("auge!cpu#3-1", "range is reversed. Did you mean #1-3?"),
        ("auge!cpu#0", "tweak numbers start at 1"),
        ("auge!cpu#1,", "expected a tweak number after ','"),
        ("auge!cpu#1-", "expected a tweak number after '-'"),
        (
            "auge!cpu#abc",
            "expected a tweak number after '#'. Example: auge!cpu#1",
        ),
        (
            "auge!cpu#1 2",
            "unexpected '2' after the tweak numbers. Separate tweak numbers with commas",
        ),
        (
            "cpu#1",
            "commands start with auge!. Did you mean auge!cpu#1?",
        ),
        (
            "auge cpu#1",
            "commands start with auge!. Did you mean auge!cpu#1?",
        ),
        ("hello there", "commands start with auge!. Try auge!help"),
        ("auge!", "type a command after auge!. Try auge!help"),
        ("", "type a command. Try auge!help"),
        ("   ", "type a command. Try auge!help"),
        (
            "auge!hlep",
            "unknown command 'hlep'. Did you mean 'auge!help'?",
        ),
        (
            "auge!qiut",
            "unknown command 'qiut'. Did you mean 'auge!quit'?",
        ),
        (
            "auge!zzzzzzzzz",
            "unknown command 'zzzzzzzzz'. Try auge!help",
        ),
        (
            "auge!undo",
            "undo needs tweaks to work on. Example: auge!undo cpu#1",
        ),
        (
            "auge!undo cpu",
            "undo needs tweak numbers. Example: auge!undo cpu#1",
        ),
        ("auge!undo all", "did you mean auge!undo-all?"),
        ("auge!quit now", "unexpected 'n' after 'quit'"),
        (
            "auge!cpu#1@cs2.exe",
            "'@' only applies to game tweaks, not cpu",
        ),
        (
            "auge!cpu 1",
            "unexpected '1' after 'cpu'. To pick tweaks use auge!cpu#1",
        ),
        (
            "auge!game#1@",
            "expected a game's .exe name. Example: cs2.exe",
        ),
    ] {
        assert_eq!(error(input), expected, "{input}");
    }
}

#[test]
fn numeric_limits() {
    assert_eq!(error("auge!cpu#99999999999"), "99999999999 is too large");
    assert_eq!(
        error("auge!cpu#1-4294967295"),
        "that selects 4294967295 tweaks; the limit is 64"
    );
    assert_eq!(
        error("auge!cpu#1-40,41-80"),
        "that selects 80 tweaks; the limit is 64"
    );
    assert_eq!(apply("auge!cpu#1-64").numbers().len(), 64);
    assert_eq!(apply("auge!cpu#1-64,1-64").numbers().len(), 64);
    // The largest value must not overflow the width calculation.
    assert_eq!(apply("auge!cpu#4294967295").numbers(), vec![u32::MAX]);
}

#[test]
fn bad_exe_names_are_rejected() {
    for name in [
        r"..\evil.exe",
        r"C:\games\cs2.exe",
        "a/b.exe",
        "cs2",
        ".exe",
        "cs2.exe;calc.exe",
        "cs2.exe\"",
        "cs2.bat",
        "ünï.exe",
    ] {
        assert!(!is_valid_exe(name), "{name}");
        assert!(parse(&format!("auge!game {name}")).is_err(), "{name}");
    }
    assert!(!is_valid_exe(&format!("{}.exe", "a".repeat(61))));
    assert!(is_valid_exe(&format!("{}.exe", "a".repeat(60))));
    assert!(is_valid_exe("My Game_2-final.v1.EXE"));
}

#[test]
fn caret_points_at_the_problem() {
    let input = "auge!cpu#3-1";
    let err = parse(input).unwrap_err();
    assert_eq!(err.span, 9..12);
    assert_eq!(
        err.render(input),
        "auge!cpu#3-1\n         ^^^\nrange is reversed. Did you mean #1-3?"
    );
    // Spans count characters, so a non-ASCII character can't push the caret off.
    let input = "auge!é";
    let err = parse(input).unwrap_err();
    assert_eq!(err.span, 5..6);
}

#[derive(Default)]
struct View {
    tweaks: BTreeMap<(Component, u32), TweakStatus>,
    game: Option<String>,
}

impl View {
    fn with(entries: &[(Component, u32)]) -> Self {
        Self {
            tweaks: entries
                .iter()
                .map(|(c, n)| ((*c, *n), TweakStatus::Active))
                .collect(),
            game: None,
        }
    }
}

impl CatalogView for View {
    fn numbers(&self, component: Component) -> Vec<u32> {
        self.tweaks
            .keys()
            .filter(|(c, _)| *c == component)
            .map(|(_, n)| *n)
            .collect()
    }

    fn status(&self, component: Component, number: u32) -> Option<TweakStatus> {
        self.tweaks.get(&(component, number)).cloned()
    }

    fn current_game(&self) -> Option<String> {
        self.game.clone()
    }
}

fn cpu_view() -> View {
    View::with(&[
        (Component::Cpu, 1),
        (Component::Cpu, 2),
        (Component::Cpu, 3),
    ])
}

#[test]
fn resolve_reports_missing_numbers_with_what_exists() {
    let target = apply("auge!cpu#7");
    let err = resolve(&target, &cpu_view(), Purpose::Apply).unwrap_err();
    assert_eq!(err.message, "tweak #7 doesn't exist in cpu (it has 1-3)");

    let mut view = cpu_view();
    view.tweaks.insert((Component::Cpu, 5), TweakStatus::Active);
    let err = resolve(&apply("auge!cpu#4"), &view, Purpose::Apply).unwrap_err();
    assert_eq!(err.message, "tweak #4 doesn't exist in cpu (it has 1-3, 5)");

    let err = resolve(&apply("auge!gpu#1"), &cpu_view(), Purpose::Apply).unwrap_err();
    assert_eq!(err.message, "gpu has no tweaks yet");
}

#[test]
fn resolve_handles_retired_tweaks() {
    let mut view = cpu_view();
    view.tweaks.insert(
        (Component::Cpu, 2),
        TweakStatus::Retired {
            reason: "no longer works".into(),
        },
    );

    let err = resolve(&apply("auge!cpu#2"), &view, Purpose::Apply).unwrap_err();
    assert_eq!(
        err.message,
        "cpu#2 was retired: no longer works. Undo still works."
    );

    let resolved = resolve(&apply("auge!cpu#1-3"), &view, Purpose::Apply).unwrap();
    assert_eq!(resolved.numbers, vec![1, 3]);
    assert_eq!(resolved.skipped, vec![(2, "no longer works".to_string())]);

    let resolved = resolve(&apply("auge!cpu#2"), &view, Purpose::UndoOrInspect).unwrap();
    assert_eq!(resolved.numbers, vec![2]);

    let mut only_retired = View::default();
    for n in [1, 2] {
        only_retired.tweaks.insert(
            (Component::Cpu, n),
            TweakStatus::Retired { reason: "x".into() },
        );
    }
    let err = resolve(&apply("auge!cpu#1-2"), &only_retired, Purpose::Apply).unwrap_err();
    assert_eq!(err.message, "every tweak selected in cpu was retired");
}

#[test]
fn resolve_requires_a_game_for_game_tweaks() {
    let mut view = View::with(&[(Component::Game, 1)]);
    let err = resolve(&apply("auge!game#1"), &view, Purpose::Apply).unwrap_err();
    assert_eq!(
        err.message,
        "no game selected. Use auge!game#1@cs2.exe or auge!game cs2.exe"
    );

    let resolved = resolve(&apply("auge!game#1@cs2.exe"), &view, Purpose::Apply).unwrap();
    assert_eq!(resolved.exe.as_deref(), Some("cs2.exe"));

    view.game = Some("dota2.exe".into());
    let resolved = resolve(&apply("auge!game#1"), &view, Purpose::Apply).unwrap();
    assert_eq!(resolved.exe.as_deref(), Some("dota2.exe"));
    // An explicit name wins over the selected game.
    let resolved = resolve(&apply("auge!game#1@cs2.exe"), &view, Purpose::Apply).unwrap();
    assert_eq!(resolved.exe.as_deref(), Some("cs2.exe"));
}

proptest::proptest! {
    #[test]
    fn parse_never_panics_and_spans_stay_in_bounds(input in "\\PC{0,80}") {
        if let Err(e) = parse(&input) {
            let len = input.chars().count();
            proptest::prop_assert!(e.span.start <= len + 1, "{e:?}");
            proptest::prop_assert!(e.span.start <= e.span.end, "{e:?}");
            let _ = e.render(&input);
        }
    }

    #[test]
    fn parse_never_panics_on_near_valid_input(
        input in "(auge!)?( |cpu|game|undo|dry|bench|ab|#|,|-|@|[0-9]{1,12}|x\\.exe){0,12}"
    ) {
        let _ = parse(&input);
    }

    #[test]
    fn selectors_expand_to_sorted_unique_numbers(
        items in proptest::collection::vec((1u32..60, 0u32..4), 1..5)
    ) {
        let text: Vec<String> = items.iter().map(|(s, w)| format!("{s}-{}", s + w)).collect();
        let target = apply(&format!("auge!cpu#{}", text.join(",")));
        let numbers = target.numbers();
        proptest::prop_assert!(numbers.windows(2).all(|w| w[0] < w[1]));
        for (start, width) in items {
            for n in start..=start + width {
                proptest::prop_assert!(numbers.contains(&n));
            }
        }
    }
}

#[test]
fn help_mentions_every_component_and_command() {
    let help = help_text();
    for component in Component::ALL {
        assert!(help.contains(component.as_str()), "{component}");
    }
    for word in all_words() {
        assert!(help.contains(word), "{word}");
    }
}

#[test]
fn every_example_in_help_parses() {
    let help = help_text();
    let examples: Vec<&str> = help
        .split_whitespace()
        .filter(|w| w.starts_with(PREFIX) && !w.contains('<'))
        .collect();
    assert!(examples.len() > 15);
    for line in help.lines() {
        // Examples may contain spaces, so try each one with the words that follow it.
        for (i, _) in line.match_indices(PREFIX) {
            let rest = &line[i..];
            let next = rest[1..].find(PREFIX).map_or(rest.len(), |n| n + 1);
            let example = rest[..next].trim();
            if !example.contains('<') && !example.contains('|') {
                assert!(parse(example).is_ok(), "{example}");
            }
        }
    }
}
