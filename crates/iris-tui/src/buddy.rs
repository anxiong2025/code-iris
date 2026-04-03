//! Buddy / Companion system — Bones + Soul architecture.
//!
//! **Bones** (deterministic, derived from seed every time):
//!   species, rarity, eye, hat, shiny, stats
//!
//! **Soul** (generated once at hatch, persisted):
//!   name, personality

use std::fmt;

// ── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rarity {
    Common,
    Uncommon,
    Rare,
    Epic,
    Legendary,
}

impl Rarity {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Common => "Common",
            Self::Uncommon => "Uncommon",
            Self::Rare => "Rare",
            Self::Epic => "Epic",
            Self::Legendary => "Legendary",
        }
    }
    pub fn color(&self) -> (u8, u8, u8) {
        match self {
            Self::Common => (140, 140, 140),
            Self::Uncommon => (100, 200, 100),
            Self::Rare => (100, 150, 255),
            Self::Epic => (180, 100, 255),
            Self::Legendary => (255, 200, 60),
        }
    }
    pub fn stars(&self) -> &'static str {
        match self {
            Self::Common => "*",
            Self::Uncommon => "**",
            Self::Rare => "***",
            Self::Epic => "****",
            Self::Legendary => "*****",
        }
    }
}

const RARITY_WEIGHTS: [(Rarity, u32); 5] = [
    (Rarity::Common, 60),
    (Rarity::Uncommon, 25),
    (Rarity::Rare, 10),
    (Rarity::Epic, 4),
    (Rarity::Legendary, 1),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Species {
    Duck, Goose, Blob, Cat, Rabbit, Mushroom, Chonk,
    Penguin, Turtle, Snail, Owl, Robot,
    Octopus, Ghost, Cactus,
    Axolotl, Dragon,
    Capybara,
}

const ALL_SPECIES: &[Species] = &[
    Species::Duck, Species::Goose, Species::Blob, Species::Cat,
    Species::Rabbit, Species::Mushroom, Species::Chonk,
    Species::Penguin, Species::Turtle, Species::Snail, Species::Owl, Species::Robot,
    Species::Octopus, Species::Ghost, Species::Cactus,
    Species::Axolotl, Species::Dragon,
    Species::Capybara,
];

impl fmt::Display for Species {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Duck => "Duck", Self::Goose => "Goose", Self::Blob => "Blob",
            Self::Cat => "Cat", Self::Rabbit => "Rabbit", Self::Mushroom => "Mushroom",
            Self::Chonk => "Chonk", Self::Penguin => "Penguin", Self::Turtle => "Turtle",
            Self::Snail => "Snail", Self::Owl => "Owl", Self::Robot => "Robot",
            Self::Octopus => "Octopus", Self::Ghost => "Ghost", Self::Cactus => "Cactus",
            Self::Axolotl => "Axolotl", Self::Dragon => "Dragon", Self::Capybara => "Capybara",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eye { Dot, Star, Cross, Circle, At, Degree }

const ALL_EYES: &[Eye] = &[Eye::Dot, Eye::Star, Eye::Cross, Eye::Circle, Eye::At, Eye::Degree];

impl Eye {
    pub fn ch(&self) -> char {
        match self {
            Self::Dot => '\u{00B7}',    // ·
            Self::Star => '\u{2726}',   // ✦
            Self::Cross => '\u{00D7}',  // ×
            Self::Circle => '\u{25C9}', // ◉
            Self::At => '@',
            Self::Degree => '\u{00B0}', // °
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hat { None, Crown, TopHat, Propeller, Halo, Wizard, Beanie }

const NON_NONE_HATS: &[Hat] = &[
    Hat::Crown, Hat::TopHat, Hat::Propeller, Hat::Halo, Hat::Wizard, Hat::Beanie,
];

impl Hat {
    /// Hat line for sprite line 0 (12 chars wide).
    pub fn sprite_line(&self) -> &'static str {
        match self {
            Self::None => "",
            Self::Crown =>    "   \\^^^/    ",
            Self::TopHat =>   "   [___]    ",
            Self::Propeller =>"    -+-     ",
            Self::Halo =>     "   (   )    ",
            Self::Wizard =>   "    /^\\     ",
            Self::Beanie =>   "   (___)    ",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatName { Debugging, Patience, Chaos, Wisdom, Snark }

const ALL_STATS: &[StatName] = &[
    StatName::Debugging, StatName::Patience, StatName::Chaos,
    StatName::Wisdom, StatName::Snark,
];

impl fmt::Display for StatName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Debugging => "DEBUGGING", Self::Patience => "PATIENCE",
            Self::Chaos => "CHAOS", Self::Wisdom => "WISDOM", Self::Snark => "SNARK",
        };
        f.write_str(s)
    }
}

// ── Bones — deterministic from seed ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CompanionBones {
    pub rarity: Rarity,
    pub species: Species,
    pub eye: Eye,
    pub hat: Hat,
    pub shiny: bool,
    pub stats: [(StatName, u32); 5],
}

// ── Soul — generated once, persisted ─────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CompanionSoul {
    pub name: String,
    pub personality: String,
}

// ── Full companion ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Companion {
    pub bones: CompanionBones,
    pub soul: CompanionSoul,
}

// ── Mulberry32 PRNG — tiny seeded, good enough for picking ducks ─────────────

struct Rng(u32);

impl Rng {
    fn new(seed: u32) -> Self { Self(seed) }

    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_add(0x6D2B79F5);
        let mut t = self.0;
        t = (t ^ (t >> 15)).wrapping_mul(1 | t);
        t = (t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t))) ^ t;
        t ^ (t >> 14)
    }

    /// Random float in [0, 1).
    fn f(&mut self) -> f64 {
        self.next() as f64 / 4_294_967_296.0
    }

    /// Pick from a slice.
    fn pick<'a, T>(&mut self, arr: &'a [T]) -> &'a T {
        let idx = (self.f() * arr.len() as f64) as usize;
        &arr[idx.min(arr.len() - 1)]
    }
}

fn hash_string(s: &str) -> u32 {
    let mut h: u32 = 2_166_136_261;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16_777_619);
    }
    h
}

// ── Roll logic ───────────────────────────────────────────────────────────────

const SALT: &str = "iris-friend-2026";

fn roll_rarity(rng: &mut Rng) -> Rarity {
    let total: u32 = RARITY_WEIGHTS.iter().map(|(_, w)| w).sum();
    let mut roll = rng.f() * total as f64;
    for (rarity, weight) in &RARITY_WEIGHTS {
        roll -= *weight as f64;
        if roll < 0.0 { return *rarity; }
    }
    Rarity::Common
}

fn rarity_stat_floor(r: Rarity) -> u32 {
    match r {
        Rarity::Common => 5,
        Rarity::Uncommon => 15,
        Rarity::Rare => 25,
        Rarity::Epic => 35,
        Rarity::Legendary => 50,
    }
}

/// One peak stat, one dump stat, rest scattered. Rarity bumps the floor.
fn roll_stats(rng: &mut Rng, rarity: Rarity) -> [(StatName, u32); 5] {
    let floor = rarity_stat_floor(rarity);
    let peak = *rng.pick(ALL_STATS);
    let mut dump = *rng.pick(ALL_STATS);
    while dump == peak { dump = *rng.pick(ALL_STATS); }

    let mut stats = [(StatName::Debugging, 0u32); 5];
    for (i, &name) in ALL_STATS.iter().enumerate() {
        let val = if name == peak {
            (floor + 50 + (rng.f() * 30.0) as u32).min(100)
        } else if name == dump {
            (floor as i32 - 10 + (rng.f() * 15.0) as i32).max(1) as u32
        } else {
            floor + (rng.f() * 40.0) as u32
        };
        stats[i] = (name, val);
    }
    stats
}

fn roll_bones(rng: &mut Rng) -> CompanionBones {
    let rarity = roll_rarity(rng);
    CompanionBones {
        rarity,
        species: *rng.pick(ALL_SPECIES),
        eye: *rng.pick(ALL_EYES),
        hat: if rarity == Rarity::Common { Hat::None } else { *rng.pick(NON_NONE_HATS) },
        shiny: rng.f() < 0.01,
        stats: roll_stats(rng, rarity),
    }
}

/// Generate a soul — simple deterministic name from seed (no LLM needed).
fn roll_soul(rng: &mut Rng, species: Species) -> CompanionSoul {
    const ADJECTIVES: &[&str] = &[
        "Brave", "Sleepy", "Tiny", "Cosmic", "Fuzzy", "Sneaky", "Mellow",
        "Spicy", "Cozy", "Zippy", "Wobbly", "Crispy", "Stormy", "Silent",
        "Noodle", "Turbo", "Pixel", "Dusty",
    ];
    const PERSONALITIES: &[&str] = &[
        "Sits quietly and watches your code scroll by.",
        "Quacks at compile errors. Surprisingly helpful.",
        "Judges your variable names in silence.",
        "Falls asleep during long builds, wakes up for tests.",
        "Tries to eat your semicolons when you're not looking.",
        "Gives disapproving looks at TODO comments.",
        "Hums quietly during successful deploys.",
        "Panics slightly whenever you force-push.",
        "Counts your coffee intake with growing concern.",
        "Secretly writes better commit messages than you.",
    ];
    let adj = rng.pick(ADJECTIVES);
    let personality = rng.pick(PERSONALITIES);
    CompanionSoul {
        name: format!("{adj} {species}"),
        personality: personality.to_string(),
    }
}

/// Roll a full companion from a seed string.
pub fn roll(seed: &str) -> Companion {
    let hash = hash_string(&format!("{seed}{SALT}"));
    let mut rng = Rng::new(hash);
    let bones = roll_bones(&mut rng);
    let soul = roll_soul(&mut rng, bones.species);
    Companion { bones, soul }
}

/// Quick roll using timestamp (for users without userId).
pub fn roll_random() -> Companion {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    roll(&nanos.to_string())
}

// ── Sprites — 3 frames per species, 5 lines × 12 chars ──────────────────────
// {E} is replaced with the rolled eye character at render time.

fn get_body_frames(species: Species) -> &'static [&'static [&'static str]] {
    match species {
        Species::Duck => &[
            &["            ", "    __      ", "  <({E} )___  ", "   (  ._>   ", "    `--'    "],
            &["            ", "    __      ", "  <({E} )___  ", "   (  ._>   ", "    `--'~   "],
            &["            ", "    __      ", "  <({E} )___  ", "   (  .__>  ", "    `--'    "],
        ],
        Species::Goose => &[
            &["            ", "     ({E}>    ", "     ||     ", "   _(__)_   ", "    ^^^^    "],
            &["            ", "    ({E}>     ", "     ||     ", "   _(__)_   ", "    ^^^^    "],
            &["            ", "     ({E}>>   ", "     ||     ", "   _(__)_   ", "    ^^^^    "],
        ],
        Species::Blob => &[
            &["            ", "   .----.   ", "  ( {E}  {E} )  ", "  (      )  ", "   `----'   "],
            &["            ", "  .------.  ", " (  {E}  {E}  ) ", " (        ) ", "  `------'  "],
            &["            ", "    .--.    ", "   ({E}  {E})   ", "   (    )   ", "    `--'    "],
        ],
        Species::Cat => &[
            &["            ", "   /\\_/\\    ", "  ( {E}   {E})  ", "  (  w  )   ", "  (\")_(\")   "],
            &["            ", "   /\\_/\\    ", "  ( {E}   {E})  ", "  (  w  )   ", "  (\")_(\")~  "],
            &["            ", "   /\\-/\\    ", "  ( {E}   {E})  ", "  (  w  )   ", "  (\")_(\")   "],
        ],
        Species::Rabbit => &[
            &["            ", "   (\\__/)   ", "  ( {E}  {E} )  ", " =(  ..  )= ", "  (\")__(\"  "],
            &["            ", "   (|__/)   ", "  ( {E}  {E} )  ", " =(  ..  )= ", "  (\")__(\")  "],
            &["            ", "   (\\__/)   ", "  ( {E}  {E} )  ", " =( .  . )= ", "  (\")__(\")  "],
        ],
        Species::Mushroom => &[
            &["            ", " .-o-OO-o-. ", "(__________)", "   |{E}  {E}|   ", "   |____|   "],
            &["            ", " .-O-oo-O-. ", "(__________)", "   |{E}  {E}|   ", "   |____|   "],
            &["   . o  .   ", " .-o-OO-o-. ", "(__________)", "   |{E}  {E}|   ", "   |____|   "],
        ],
        Species::Chonk => &[
            &["            ", "  /\\    /\\  ", " ( {E}    {E} ) ", " (   ..   ) ", "  `------'  "],
            &["            ", "  /\\    /|  ", " ( {E}    {E} ) ", " (   ..   ) ", "  `------'  "],
            &["            ", "  /\\    /\\  ", " ( {E}    {E} ) ", " (   ..   ) ", "  `------'~ "],
        ],
        Species::Penguin => &[
            &["            ", "  .---.     ", "  ({E}>{E})     ", " /(   )\\    ", "  `---'     "],
            &["            ", "  .---.     ", "  ({E}>{E})     ", " |(   )|    ", "  `---'     "],
            &["  .---.     ", "  ({E}>{E})     ", " /(   )\\    ", "  `---'     ", "   ~ ~      "],
        ],
        Species::Turtle => &[
            &["            ", "   _,--._   ", "  ( {E}  {E} )  ", " /[______]\\ ", "  ``    ``  "],
            &["            ", "   _,--._   ", "  ( {E}  {E} )  ", " /[______]\\ ", "   ``  ``   "],
            &["            ", "   _,--._   ", "  ( {E}  {E} )  ", " /[======]\\ ", "  ``    ``  "],
        ],
        Species::Snail => &[
            &["            ", " {E}    .--.  ", "  \\  ( @ )  ", "   \\_`--'   ", "  ~~~~~~~   "],
            &["            ", "  {E}   .--.  ", "  |  ( @ )  ", "   \\_`--'   ", "  ~~~~~~~   "],
            &["            ", " {E}    .--.  ", "  \\  ( @  ) ", "   \\_`--'   ", "   ~~~~~~   "],
        ],
        Species::Owl => &[
            &["            ", "   /\\  /\\   ", "  (({E})({E}))  ", "  (  ><  )  ", "   `----'   "],
            &["            ", "   /\\  /\\   ", "  (({E})({E}))  ", "  (  ><  )  ", "   .----.   "],
            &["            ", "   /\\  /\\   ", "  (({E})(-))  ", "  (  ><  )  ", "   `----'   "],
        ],
        Species::Robot => &[
            &["            ", "   .[||].   ", "  [ {E}  {E} ]  ", "  [ ==== ]  ", "  `------'  "],
            &["            ", "   .[||].   ", "  [ {E}  {E} ]  ", "  [ -==- ]  ", "  `------'  "],
            &["     *      ", "   .[||].   ", "  [ {E}  {E} ]  ", "  [ ==== ]  ", "  `------'  "],
        ],
        Species::Octopus => &[
            &["            ", "   .----.   ", "  ( {E}  {E} )  ", "  (______)  ", "  /\\/\\/\\/\\  "],
            &["            ", "   .----.   ", "  ( {E}  {E} )  ", "  (______)  ", "  \\/\\/\\/\\/  "],
            &["     o      ", "   .----.   ", "  ( {E}  {E} )  ", "  (______)  ", "  /\\/\\/\\/\\  "],
        ],
        Species::Ghost => &[
            &["            ", "   .----.   ", "  / {E}  {E} \\  ", "  |      |  ", "  ~`~``~`~  "],
            &["            ", "   .----.   ", "  / {E}  {E} \\  ", "  |      |  ", "  `~`~~`~`  "],
            &["    ~  ~    ", "   .----.   ", "  / {E}  {E} \\  ", "  |      |  ", "  ~~`~~`~~  "],
        ],
        Species::Cactus => &[
            &["            ", " n  ____  n ", " | |{E}  {E}| | ", " |_|    |_| ", "   |    |   "],
            &["            ", "    ____    ", " n |{E}  {E}| n ", " |_|    |_| ", "   |    |   "],
            &[" n        n ", " |  ____  | ", " | |{E}  {E}| | ", " |_|    |_| ", "   |    |   "],
        ],
        Species::Axolotl => &[
            &["            ", "}~(______)~{", "}~({E} .. {E})~{", "  ( .--. )  ", "  (_/  \\_)  "],
            &["            ", "~}(______){~", "~}({E} .. {E}){~", "  ( .--. )  ", "  (_/  \\_)  "],
            &["            ", "}~(______)~{", "}~({E} .. {E})~{", "  (  --  )  ", "  ~_/  \\_~  "],
        ],
        Species::Dragon => &[
            &["            ", "  /^\\  /^\\  ", " <  {E}  {E}  > ", " (   ~~   ) ", "  `-vvvv-'  "],
            &["            ", "  /^\\  /^\\  ", " <  {E}  {E}  > ", " (        ) ", "  `-vvvv-'  "],
            &["   ~    ~   ", "  /^\\  /^\\  ", " <  {E}  {E}  > ", " (   ~~   ) ", "  `-vvvv-'  "],
        ],
        Species::Capybara => &[
            &["            ", "  n______n  ", " ( {E}    {E} ) ", " (   oo   ) ", "  `------'  "],
            &["            ", "  n______n  ", " ( {E}    {E} ) ", " (   Oo   ) ", "  `------'  "],
            &["    ~  ~    ", "  u______n  ", " ( {E}    {E} ) ", " (   oo   ) ", "  `------'  "],
        ],
    }
}

/// Idle animation sequence — mostly frame 0 (rest), occasional fidget.
/// -1 = blink on frame 0.
const IDLE_SEQUENCE: &[i8] = &[0, 0, 0, 0, 1, 0, 0, 0, -1, 0, 0, 2, 0, 0, 0];

/// Render a full 5-line sprite with eye substitution, hat, and shiny marker.
pub fn render_sprite(bones: &CompanionBones, tick: u64) -> Vec<String> {
    let seq_idx = (tick as usize / 3) % IDLE_SEQUENCE.len();
    let frame_idx = IDLE_SEQUENCE[seq_idx];
    let frames = get_body_frames(bones.species);

    let actual_frame = if frame_idx < 0 { 0usize } else { frame_idx as usize % frames.len() };
    let body = frames[actual_frame];

    let eye_char = if frame_idx < 0 { '-' } else { bones.eye.ch() };
    let eye_str = eye_char.to_string();

    let mut lines: Vec<String> = body.iter()
        .map(|line| line.replace("{E}", &eye_str))
        .collect();

    // Hat on line 0 if blank and hat is not None.
    if bones.hat != Hat::None && lines[0].trim().is_empty() {
        let hat_line = bones.hat.sprite_line();
        if !hat_line.is_empty() {
            lines[0] = hat_line.to_string();
        }
    }

    // Drop blank hat slot if ALL frames have blank line 0 (no smoke/antenna).
    if lines[0].trim().is_empty() && frames.iter().all(|f| f[0].trim().is_empty()) {
        lines.remove(0);
    }

    // Shiny marker.
    if bones.shiny {
        if let Some(last) = lines.last_mut() {
            last.push_str(" +");
        }
    }

    lines
}

/// Render a compact face for status bar display.
pub fn render_face(bones: &CompanionBones) -> String {
    let e = bones.eye.ch();
    match bones.species {
        Species::Duck | Species::Goose => format!("({e}>"),
        Species::Blob => format!("({e}{e})"),
        Species::Cat => format!("={e}w{e}="),
        Species::Dragon => format!("<{e}~{e}>"),
        Species::Octopus => format!("~({e}{e})~"),
        Species::Owl => format!("({e})({e})"),
        Species::Penguin => format!("({e}>)"),
        Species::Turtle => format!("[{e}_{e}]"),
        Species::Snail => format!("{e}(@)"),
        Species::Ghost => format!("/{e}{e}\\"),
        Species::Axolotl => format!("}}{e}.{e}{{"),
        Species::Capybara => format!("({e}oo{e})"),
        Species::Cactus | Species::Mushroom => format!("|{e}  {e}|"),
        Species::Robot => format!("[{e}{e}]"),
        Species::Rabbit => format!("({e}..{e})"),
        Species::Chonk => format!("({e}.{e})"),
    }
}

/// Animated face for status bar — cycles through idle sequence.
pub fn animated_face(bones: &CompanionBones, tick: u64) -> String {
    let seq_idx = (tick as usize / 3) % IDLE_SEQUENCE.len();
    let frame_idx = IDLE_SEQUENCE[seq_idx];
    if frame_idx < 0 {
        // Blink frame — replace eye with -
        let e = '-';
        match bones.species {
            Species::Duck | Species::Goose => format!("({e}>"),
            Species::Cat => format!("={e}w{e}="),
            _ => render_face(bones),
        }
    } else {
        render_face(bones)
    }
}

/// Format the full reveal card for `/buddy` display.
pub fn format_reveal_card(companion: &Companion) -> String {
    let b = &companion.bones;
    let s = &companion.soul;
    let stars = b.rarity.stars();
    let rarity = b.rarity.label();
    let shiny_tag = if b.shiny { " [SHINY!]" } else { "" };

    let sprite = render_sprite(b, 0);

    let mut lines = Vec::new();
    lines.push(String::new());
    lines.push(format!("  {stars}  {name}  ({species}){shiny_tag}",
        name = s.name, species = b.species));
    lines.push(format!("  [{rarity}]  Eye: {eye}",
        eye = b.eye.ch()));
    if b.hat != Hat::None {
        lines.push(format!("  Hat: {:?}", b.hat));
    }
    lines.push(String::new());

    // Sprite art
    for sl in &sprite {
        lines.push(format!("  {sl}"));
    }
    lines.push(String::new());

    // Personality
    lines.push(format!("  \"{personality}\"", personality = s.personality));
    lines.push(String::new());

    // Stats bars
    for (name, val) in &b.stats {
        let bar_len = (*val as usize) / 5;
        let bar: String = "|".repeat(bar_len);
        let pad: String = " ".repeat(20usize.saturating_sub(bar_len));
        lines.push(format!("  {:<10} [{bar}{pad}] {val}", format!("{name}")));
    }
    lines.push(String::new());

    // Footer
    lines.push(format!("  {} is now watching your code.", s.name));
    lines.push(String::new());

    lines.join("\n")
}

// ── Persistence — save/load companion seed to ~/.iris/companion ──────────────

fn companion_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".iris").join("companion"))
}

/// Save the companion's seed so it can be restored on next startup.
pub fn save_companion(seed: &str) {
    let Some(path) = companion_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, seed);
}

/// Load a previously saved companion. Returns None if no companion file exists.
pub fn load_companion() -> Option<Companion> {
    let path = companion_path()?;
    let seed = std::fs::read_to_string(&path).ok()?;
    let seed = seed.trim();
    if seed.is_empty() { return None; }
    Some(roll(seed))
}

/// Roll a new random companion AND persist the seed.
// ── Reaction system — buddy reacts to coding events ─────────────────────────

/// Events the buddy can react to.
#[derive(Debug, Clone, Copy)]
pub enum BuddyEvent {
    /// Agent started thinking.
    Thinking,
    /// A tool was called.
    ToolCall,
    /// Agent finished responding.
    Done,
    /// An error occurred.
    Error,
    /// User has been idle for a while.
    Idle,
    /// Agent is streaming text.
    Streaming,
}

/// Pick a reaction message for the given event. Returns None sometimes
/// (buddy doesn't react to every event — ~40% chance for most events).
pub fn pick_reaction(event: BuddyEvent, bones: &CompanionBones, tick: u64) -> Option<&'static str> {
    // Use tick as entropy source — deterministic per frame but varied
    let roll = ((tick.wrapping_mul(2654435761)) >> 16) % 100;

    let (chance, pool): (u64, &[&str]) = match event {
        BuddyEvent::Thinking => (30, &[
            "*watches intently*",
            "*tilts head*",
            "hmm...",
            "*thinking too*",
            "let me see...",
            "*leans closer*",
            "interesting...",
        ]),
        BuddyEvent::ToolCall => (25, &[
            "*peeks at terminal*",
            "ooh, running stuff",
            "*watches code scroll*",
            "beep boop",
            "*takes notes*",
            "working...",
        ]),
        BuddyEvent::Done => (50, &[
            "*nods approvingly*",
            "nice!",
            "*happy wiggle*",
            "done~",
            "*stretches*",
            "good job!",
            "*yawns contentedly*",
        ]),
        BuddyEvent::Error => (70, &[
            "*concerned look*",
            "uh oh...",
            "*pats you gently*",
            "it happens...",
            "*hides behind screen*",
            "we can fix this!",
            "*offers moral support*",
        ]),
        BuddyEvent::Idle => (15, &[
            "*snores quietly*",
            "zzz...",
            "*fidgets*",
            "*looks around*",
            "*yawns*",
            "...",
            "*blinks slowly*",
        ]),
        BuddyEvent::Streaming => (10, &[
            "*reads along*",
            "*nods*",
            "mhm mhm",
        ]),
    };

    if roll >= chance { return None; }

    // Pick from pool using tick + species as seed
    let idx = ((tick as usize).wrapping_add(bones.species as usize)) % pool.len();
    Some(pool[idx])
}

/// State for the speech bubble display.
#[derive(Debug, Clone)]
pub struct ReactionState {
    pub text: String,
    /// Ticks remaining before the bubble fades.
    pub ticks_remaining: u16,
}

impl ReactionState {
    pub fn new(text: &str) -> Self {
        Self { text: text.to_string(), ticks_remaining: 20 } // ~10s at 500ms/tick
    }

    /// Returns true if the bubble should still be shown.
    pub fn is_visible(&self) -> bool {
        self.ticks_remaining > 0
    }

    /// Returns true if in the fade window (last 3s).
    pub fn is_fading(&self) -> bool {
        self.ticks_remaining > 0 && self.ticks_remaining <= 6
    }

    /// Decrement tick. Call once per animation tick.
    pub fn tick(&mut self) {
        self.ticks_remaining = self.ticks_remaining.saturating_sub(1);
    }
}

pub fn roll_and_save() -> (Companion, String) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seed = nanos.to_string();
    let companion = roll(&seed);
    save_companion(&seed);
    (companion, seed)
}
