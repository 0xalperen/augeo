//! The `auge!` command grammar: a tokenizing parser and a resolver.
//!
//! [`parse`] checks syntax only. [`resolve`] checks a target against the catalog, so this crate needs
//! no catalog dependency. Neither one performs an action: a parsed apply command only ever leads to
//! the confirmation screen.
//!
//! ```text
//! input     = ws "auge!" command ws ;
//! command   = "help" [ws1 topic] | "quit" | "check" | "myths" | "log"
//!           | "status" [ws1 component]
//!           | "undo" ws1 target | "undo-all"
//!           | "dry" ws1 (target | preset)
//!           | "info" ws1 target
//!           | "preset" ws1 name
//!           | "bench" [ws1 bench_args]
//!           | "game" ws1 exe
//!           | "timer" | "polling" | "restore-point" | "elevate" | "reboot"
//!           | component
//!           | target ;
//! target    = component "#" selector ["@" exe] ;
//! selector  = item {"," item} ;
//! item      = number ["-" number] ;
//! bench_args = "start" | "list" | "aa" | "compare" ws1 id ws1 id | "ab" ws1 target ;
//! ```

#![forbid(unsafe_code)]

use std::{collections::BTreeSet, fmt, ops::Range};

/// Every command starts with this.
pub const PREFIX: &str = "auge!";
/// The most tweaks one target may select.
pub const MAX_SELECTION: usize = 64;
/// The longest accepted executable name, including `.exe`.
pub const MAX_EXE_LEN: usize = 64;

/// A tweak component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Component {
    Cpu,
    Gpu,
    Game,
    Display,
    Input,
    Network,
    Memory,
    Power,
    Storage,
    System,
}

impl Component {
    /// All components, in display order.
    pub const ALL: [Component; 10] = [
        Component::Cpu,
        Component::Gpu,
        Component::Game,
        Component::Display,
        Component::Input,
        Component::Network,
        Component::Memory,
        Component::Power,
        Component::Storage,
        Component::System,
    ];

    /// The name used in commands and catalog files.
    pub fn as_str(self) -> &'static str {
        match self {
            Component::Cpu => "cpu",
            Component::Gpu => "gpu",
            Component::Game => "game",
            Component::Display => "display",
            Component::Input => "input",
            Component::Network => "network",
            Component::Memory => "memory",
            Component::Power => "power",
            Component::Storage => "storage",
            Component::System => "system",
        }
    }

    /// Looks up a component by its exact lowercase name.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == name)
    }
}

impl fmt::Display for Component {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One item of a selector: a single number or an inclusive range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorItem {
    pub start: u32,
    pub end: u32,
    /// Character positions of this item in the input.
    pub span: Range<usize>,
}

impl SelectorItem {
    fn is_range(&self) -> bool {
        self.start != self.end
    }
}

/// `component#selector[@exe]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub component: Component,
    pub items: Vec<SelectorItem>,
    pub exe: Option<String>,
    /// Character positions of the whole target in the input.
    pub span: Range<usize>,
}

impl Target {
    /// The selected numbers in ascending order, without duplicates.
    pub fn numbers(&self) -> Vec<u32> {
        let set: BTreeSet<u32> = self.items.iter().flat_map(|i| i.start..=i.end).collect();
        set.into_iter().collect()
    }
}

/// What `auge!dry` previews.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DryTarget {
    Tweaks(Target),
    Preset(String),
}

/// Arguments of `auge!bench`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BenchArgs {
    Open,
    Start,
    List,
    NoiseFloor,
    Compare(String, String),
    AbTest(Target),
}

/// A syntactically valid command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help(Option<String>),
    Quit,
    Check,
    Myths,
    Log,
    Status(Option<Component>),
    Undo(Target),
    UndoAll,
    Dry(DryTarget),
    Info(Target),
    Preset(String),
    Bench(BenchArgs),
    SelectGame(String),
    Timer,
    Polling,
    RestorePoint,
    Elevate,
    Reboot,
    Open(Component),
    Apply(Target),
}

/// A parse or resolve failure, with the input positions it refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandError {
    pub message: String,
    /// Character positions in the input.
    pub span: Range<usize>,
}

impl CommandError {
    fn new(message: impl Into<String>, span: Range<usize>) -> Self {
        Self {
            message: message.into(),
            span,
        }
    }

    /// The input with a caret line under the problem, then the message.
    pub fn render(&self, input: &str) -> String {
        let width = self.span.end.saturating_sub(self.span.start).max(1);
        format!(
            "{input}\n{}{}\n{}",
            " ".repeat(self.span.start),
            "^".repeat(width),
            self.message
        )
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CommandError {}

const COMMAND_WORDS: [&str; 16] = [
    "help",
    "quit",
    "check",
    "myths",
    "log",
    "status",
    "undo",
    "undo-all",
    "dry",
    "info",
    "preset",
    "bench",
    "timer",
    "polling",
    "restore-point",
    "elevate",
];
const COMMAND_WORD_REBOOT: &str = "reboot";

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let current = row[j + 1];
            row[j + 1] = if ca == cb {
                previous
            } else {
                1 + previous.min(current).min(row[j])
            };
            previous = current;
        }
    }
    row[b.len()]
}

fn closest<'a>(word: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    candidates
        .into_iter()
        .map(|c| (edit_distance(word, c), c))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

fn all_words() -> impl Iterator<Item = &'static str> {
    COMMAND_WORDS
        .into_iter()
        .chain([COMMAND_WORD_REBOOT])
        .chain(Component::ALL.into_iter().map(Component::as_str))
}

/// Checks an executable name: `^[A-Za-z0-9._ -]{1,64}\.exe$`, case-insensitive on the extension.
pub fn is_valid_exe(name: &str) -> bool {
    name.len() <= MAX_EXE_LEN
        && name.len() > ".exe".len()
        && name.to_ascii_lowercase().ends_with(".exe")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ' ' | '-'))
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn at_end(&self) -> bool {
        self.pos >= self.chars.len()
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// Reads a lowercased word of letters, digits and hyphens.
    fn word(&mut self) -> (String, Range<usize>) {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            self.pos += 1;
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        (text.to_ascii_lowercase(), start..self.pos)
    }

    /// Takes the rest of the input without surrounding whitespace.
    fn rest(&mut self) -> (String, Range<usize>) {
        self.skip_ws();
        let start = self.pos;
        let mut end = self.chars.len();
        while end > start && self.chars[end - 1].is_whitespace() {
            end -= 1;
        }
        self.pos = self.chars.len();
        (self.chars[start..end].iter().collect(), start..end)
    }

    fn here(&self) -> Range<usize> {
        self.pos..self.pos + 1
    }

    fn number(&mut self, after: &str) -> Result<(u32, Range<usize>), CommandError> {
        self.skip_ws();
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if start == self.pos {
            let example = if after == "'#'" {
                ". Example: auge!cpu#1"
            } else {
                ""
            };
            return Err(CommandError::new(
                format!("expected a tweak number after {after}{example}"),
                self.here(),
            ));
        }
        let span = start..self.pos;
        let text: String = self.chars[span.clone()].iter().collect();
        let value = text
            .parse::<u32>()
            .map_err(|_| CommandError::new(format!("{text} is too large"), span.clone()))?;
        if value == 0 {
            return Err(CommandError::new("tweak numbers start at 1", span));
        }
        Ok((value, span))
    }

    fn selector(&mut self) -> Result<Vec<SelectorItem>, CommandError> {
        let mut items = Vec::new();
        let mut after = "'#'";
        loop {
            let (start, start_span) = self.number(after)?;
            self.skip_ws();
            let item = if self.eat('-') {
                let (end, end_span) = self.number("'-'")?;
                let span = start_span.start..end_span.end;
                if end < start {
                    return Err(CommandError::new(
                        format!("range is reversed. Did you mean #{end}-{start}?"),
                        span,
                    ));
                }
                SelectorItem { start, end, span }
            } else {
                SelectorItem {
                    start,
                    end: start,
                    span: start_span,
                }
            };
            let width = u64::from(item.end) - u64::from(item.start) + 1;
            if width > MAX_SELECTION as u64 {
                return Err(CommandError::new(
                    format!("that selects {width} tweaks; the limit is {MAX_SELECTION}"),
                    item.span,
                ));
            }
            items.push(item);
            self.skip_ws();
            if !self.eat(',') {
                break;
            }
            after = "','";
        }
        let unique: BTreeSet<u32> = items.iter().flat_map(|i| i.start..=i.end).collect();
        if unique.len() > MAX_SELECTION {
            let span = items[0].span.start..self.pos;
            return Err(CommandError::new(
                format!(
                    "that selects {} tweaks; the limit is {MAX_SELECTION}",
                    unique.len()
                ),
                span,
            ));
        }
        Ok(items)
    }

    fn exe(&mut self) -> Result<String, CommandError> {
        let (name, span) = self.rest();
        if name.is_empty() {
            return Err(CommandError::new(
                "expected a game's .exe name. Example: cs2.exe",
                self.here(),
            ));
        }
        if !is_valid_exe(&name) {
            return Err(CommandError::new(
                format!(
                    "'{name}' isn't a valid game name. Use a file name ending in .exe, \
                     up to {MAX_EXE_LEN} characters, with no folder"
                ),
                span,
            ));
        }
        Ok(name)
    }

    /// Parses `#selector[@exe]` when the component has been read and `#` is next.
    fn target_after_component(
        &mut self,
        component: Component,
        start: usize,
    ) -> Result<Target, CommandError> {
        let _ = self.eat('#');
        let items = self.selector()?;
        self.skip_ws();
        let exe = if self.peek() == Some('@') {
            if component != Component::Game {
                return Err(CommandError::new(
                    format!("'@' only applies to game tweaks, not {component}"),
                    self.here(),
                ));
            }
            self.pos += 1;
            Some(self.exe()?)
        } else {
            None
        };
        let span = start..self.pos;
        self.finish("the tweak numbers")?;
        Ok(Target {
            component,
            items,
            exe,
            span,
        })
    }

    /// Parses a required target for commands such as `undo`.
    fn target(&mut self, verb: &str) -> Result<Target, CommandError> {
        self.skip_ws();
        let example = format!("auge!{verb} cpu#1");
        if self.at_end() {
            return Err(CommandError::new(
                format!("{verb} needs tweaks to work on. Example: {example}"),
                self.here(),
            ));
        }
        let start = self.pos;
        let (word, span) = self.word();
        let Some(component) = Component::from_name(&word) else {
            return Err(unknown_component(&word, span));
        };
        self.skip_ws();
        if self.peek() != Some('#') {
            return Err(CommandError::new(
                format!("{verb} needs tweak numbers. Example: {example}"),
                self.here(),
            ));
        }
        self.target_after_component(component, start)
    }

    fn finish(&mut self, after: &str) -> Result<(), CommandError> {
        self.skip_ws();
        if self.at_end() {
            return Ok(());
        }
        let found = self.peek().unwrap_or(' ');
        let hint = if found.is_ascii_digit() {
            ". Separate tweak numbers with commas"
        } else {
            ""
        };
        Err(CommandError::new(
            format!("unexpected '{found}' after {after}{hint}"),
            self.pos..self.chars.len(),
        ))
    }

    fn id(&mut self, what: &str) -> Result<String, CommandError> {
        self.skip_ws();
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        {
            self.pos += 1;
        }
        if start == self.pos {
            return Err(CommandError::new(format!("expected {what}"), self.here()));
        }
        Ok(self.chars[start..self.pos].iter().collect())
    }

    fn bench(&mut self) -> Result<BenchArgs, CommandError> {
        self.skip_ws();
        if self.at_end() {
            return Ok(BenchArgs::Open);
        }
        let (word, span) = self.word();
        let args = match word.as_str() {
            "start" => BenchArgs::Start,
            "list" => BenchArgs::List,
            "aa" => BenchArgs::NoiseFloor,
            "compare" => {
                let first = self.id("two run ids. Example: auge!bench compare 12 13")?;
                let second = self.id("a second run id. Example: auge!bench compare 12 13")?;
                BenchArgs::Compare(first, second)
            }
            "ab" => return Ok(BenchArgs::AbTest(self.target("bench ab")?)),
            _ => {
                let words = ["start", "list", "aa", "compare", "ab"];
                let hint = closest(&word, words)
                    .map(|s| format!(" Did you mean '{s}'?"))
                    .unwrap_or_default();
                return Err(CommandError::new(
                    format!(
                        "unknown bench option '{word}'.{hint} Options: start, list, aa, compare, ab"
                    ),
                    span,
                ));
            }
        };
        self.finish(&format!("'{word}'"))?;
        Ok(args)
    }

    fn body(&mut self) -> Result<Command, CommandError> {
        self.skip_ws();
        if self.at_end() {
            return Err(CommandError::new(
                "type a command after auge!. Try auge!help",
                self.here(),
            ));
        }
        let start = self.pos;
        let (word, span) = self.word();
        if word.is_empty() {
            return Err(CommandError::new(
                format!("unexpected '{}'. Try auge!help", self.peek().unwrap_or(' ')),
                self.here(),
            ));
        }

        let simple = match word.as_str() {
            "quit" => Some(Command::Quit),
            "check" => Some(Command::Check),
            "myths" => Some(Command::Myths),
            "log" => Some(Command::Log),
            "undo-all" => Some(Command::UndoAll),
            "timer" => Some(Command::Timer),
            "polling" => Some(Command::Polling),
            "restore-point" => Some(Command::RestorePoint),
            "elevate" => Some(Command::Elevate),
            "reboot" => Some(Command::Reboot),
            _ => None,
        };
        if let Some(command) = simple {
            self.finish(&format!("'{word}'"))?;
            return Ok(command);
        }

        match word.as_str() {
            "help" => {
                let (topic, _) = self.rest();
                Ok(Command::Help((!topic.is_empty()).then_some(topic)))
            }
            "status" => {
                self.skip_ws();
                if self.at_end() {
                    return Ok(Command::Status(None));
                }
                let (name, name_span) = self.word();
                let component = Component::from_name(&name)
                    .ok_or_else(|| unknown_component(&name, name_span))?;
                self.finish(&format!("'{name}'"))?;
                Ok(Command::Status(Some(component)))
            }
            "undo" => {
                self.skip_ws();
                if self.chars[self.pos..]
                    .iter()
                    .collect::<String>()
                    .trim()
                    .eq_ignore_ascii_case("all")
                {
                    return Err(CommandError::new(
                        "did you mean auge!undo-all?",
                        self.pos..self.chars.len(),
                    ));
                }
                Ok(Command::Undo(self.target("undo")?))
            }
            "info" => Ok(Command::Info(self.target("info")?)),
            "dry" => {
                self.skip_ws();
                let checkpoint = self.pos;
                let (name, _) = self.word();
                if Component::from_name(&name).is_some() {
                    self.pos = checkpoint;
                    return Ok(Command::Dry(DryTarget::Tweaks(self.target("dry")?)));
                }
                self.pos = checkpoint;
                if self.at_end() {
                    return Err(CommandError::new(
                        "dry needs tweaks or a preset. Example: auge!dry cpu#1",
                        self.here(),
                    ));
                }
                let name = self.id("tweaks or a preset. Example: auge!dry cpu#1")?;
                self.finish("the preset name")?;
                Ok(Command::Dry(DryTarget::Preset(name.to_ascii_lowercase())))
            }
            "preset" => {
                let name = self.id("a preset name. Example: auge!preset recommended")?;
                self.finish("the preset name")?;
                Ok(Command::Preset(name.to_ascii_lowercase()))
            }
            "bench" => Ok(Command::Bench(self.bench()?)),
            other => {
                let Some(component) = Component::from_name(other) else {
                    self.skip_ws();
                    let is_target = self.peek() == Some('#');
                    return Err(if is_target {
                        unknown_component(other, span)
                    } else {
                        unknown_command(other, span)
                    });
                };
                self.skip_ws();
                if self.at_end() {
                    return Ok(Command::Open(component));
                }
                if self.peek() == Some('#') {
                    return Ok(Command::Apply(
                        self.target_after_component(component, start)?,
                    ));
                }
                if component == Component::Game {
                    return Ok(Command::SelectGame(self.exe()?));
                }
                let found: String = self.chars[self.pos..].iter().collect();
                Err(CommandError::new(
                    format!(
                        "unexpected '{}' after '{other}'. To pick tweaks use auge!{other}#1",
                        found.trim_end()
                    ),
                    self.pos..self.chars.len(),
                ))
            }
        }
    }
}

fn unknown_component(word: &str, span: Range<usize>) -> CommandError {
    let names = Component::ALL.into_iter().map(Component::as_str);
    let hint = closest(word, names)
        .map(|s| format!(" Did you mean '{s}'?"))
        .unwrap_or_else(|| {
            format!(
                " Components: {}",
                Component::ALL.map(Component::as_str).join(", ")
            )
        });
    CommandError::new(format!("unknown component '{word}'.{hint}"), span)
}

fn unknown_command(word: &str, span: Range<usize>) -> CommandError {
    let hint = closest(word, all_words())
        .map(|s| format!(" Did you mean 'auge!{s}'?"))
        .unwrap_or_else(|| " Try auge!help".to_string());
    CommandError::new(format!("unknown command '{word}'.{hint}"), span)
}

fn parse_body(chars: Vec<char>, pos: usize) -> Result<Command, CommandError> {
    Parser { chars, pos }.body()
}

/// Parses one line from the command bar. Whitespace and keyword case are ignored.
pub fn parse(input: &str) -> Result<Command, CommandError> {
    let chars: Vec<char> = input.chars().collect();
    let start = chars.iter().take_while(|c| c.is_whitespace()).count();
    let prefix_len = PREFIX.chars().count();
    let head: String = chars.iter().skip(start).take(prefix_len).collect();
    if head.eq_ignore_ascii_case(PREFIX) {
        return parse_body(chars, start + prefix_len);
    }

    let trimmed = input.trim();
    let whole = start..chars.len();
    if trimmed.is_empty() {
        return Err(CommandError::new(
            "type a command. Try auge!help",
            0..chars.len().max(1),
        ));
    }
    // Forgot the prefix, or wrote it without the '!'. Suggest only when the fix would work.
    let lower = trimmed.to_ascii_lowercase();
    let body = match lower.strip_prefix("auge") {
        Some(rest) if rest.starts_with([' ', ':', '.', '#']) => rest[1..].trim_start(),
        Some("") => "help",
        _ => trimmed,
    };
    let fixed = format!("{PREFIX}{body}");
    let message = if parse(&fixed).is_ok() {
        format!("commands start with {PREFIX}. Did you mean {fixed}?")
    } else {
        format!("commands start with {PREFIX}. Try auge!help")
    };
    Err(CommandError::new(message, whole))
}

/// What the catalog says about one tweak number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TweakStatus {
    Active,
    Retired { reason: String },
}

/// The part of the catalog the resolver needs.
pub trait CatalogView {
    /// All numbers in a component, including retired ones.
    fn numbers(&self, component: Component) -> Vec<u32>;
    fn status(&self, component: Component, number: u32) -> Option<TweakStatus>;
    /// The game chosen with `auge!game`, if any.
    fn current_game(&self) -> Option<String>;
}

/// Why a target is being resolved. Retired tweaks can still be inspected and undone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Apply,
    UndoOrInspect,
}

/// A target checked against the catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub component: Component,
    pub numbers: Vec<u32>,
    pub exe: Option<String>,
    /// Retired tweaks a range passed over, with the reason, so the UI can show them.
    pub skipped: Vec<(u32, String)>,
}

fn describe_numbers(numbers: &[u32]) -> String {
    let mut sorted = numbers.to_vec();
    sorted.sort_unstable();
    let mut parts: Vec<String> = Vec::new();
    let mut iter = sorted.into_iter().peekable();
    while let Some(start) = iter.next() {
        let mut end = start;
        while iter.peek() == Some(&(end + 1)) {
            end += 1;
            iter.next();
        }
        parts.push(if start == end {
            start.to_string()
        } else {
            format!("{start}-{end}")
        });
    }
    parts.join(", ")
}

/// Checks a target against the catalog and returns the tweaks it really selects.
pub fn resolve(
    target: &Target,
    view: &dyn CatalogView,
    purpose: Purpose,
) -> Result<Resolved, CommandError> {
    let component = target.component;
    let known = view.numbers(component);
    let mut numbers = BTreeSet::new();
    let mut skipped = Vec::new();

    for item in &target.items {
        for number in item.start..=item.end {
            let Some(status) = view.status(component, number) else {
                let message = if known.is_empty() {
                    format!("{component} has no tweaks yet")
                } else {
                    format!(
                        "tweak #{number} doesn't exist in {component} (it has {})",
                        describe_numbers(&known)
                    )
                };
                return Err(CommandError::new(message, item.span.clone()));
            };
            match status {
                TweakStatus::Retired { reason } if purpose == Purpose::Apply => {
                    if !item.is_range() {
                        return Err(CommandError::new(
                            format!(
                                "{component}#{number} was retired: {reason}. Undo still works."
                            ),
                            item.span.clone(),
                        ));
                    }
                    skipped.push((number, reason));
                }
                _ => {
                    numbers.insert(number);
                }
            }
        }
    }
    if numbers.is_empty() {
        return Err(CommandError::new(
            format!("every tweak selected in {component} was retired"),
            target.span.clone(),
        ));
    }

    let exe = if component == Component::Game {
        let chosen = target.exe.clone().or_else(|| view.current_game());
        if chosen.is_none() {
            return Err(CommandError::new(
                format!(
                    "no game selected. Use auge!game#{0}@cs2.exe or auge!game cs2.exe",
                    numbers.first().copied().unwrap_or(1)
                ),
                target.span.clone(),
            ));
        }
        chosen
    } else {
        None
    };

    Ok(Resolved {
        component,
        numbers: numbers.into_iter().collect(),
        exe,
        skipped,
    })
}

/// The text behind `auge!help`.
pub fn help_text() -> String {
    format!(
        "\
Open a component   auge!<component>      auge!cpu
Apply tweaks       auge!cpu#1            auge!cpu#1-3   auge!cpu#1,3   auge!cpu#1-3,5
Per-game tweaks    auge!game cs2.exe     auge!game#1@cs2.exe
Preview only       auge!dry cpu#1-3      auge!dry recommended
Undo               auge!undo cpu#1       auge!undo-all
Details            auge!info cpu#1       auge!status    auge!status cpu
System report      auge!check            auge!myths     auge!log
Benchmark          auge!bench            auge!bench start | list | aa
                   auge!bench compare 12 13          auge!bench ab gpu#3
Preset             auge!preset recommended
Tools              auge!timer            auge!polling   auge!restore-point
App                auge!elevate          auge!reboot    auge!quit    auge!help

Components: {}
Applying always shows the exact changes and asks you to confirm first.",
        Component::ALL.map(Component::as_str).join(", ")
    )
}

#[cfg(test)]
mod tests;
