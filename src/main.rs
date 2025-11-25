use {
    color_eyre::{
        Result,
        eyre::{self, Context},
    },
    demand::{DemandOption, Input, MultiSelect, Select},
    dotenvy::dotenv,
    rosu_pp::{Beatmap as PpBeatmap, Performance, model::mode::GameMode as PpGameMode},
    rosu_v2::prelude::*,
    std::{env, fmt::Display},
};

#[derive(Clone, Copy, Debug)]
enum DetailedJudgements {
    Osu {
        n300: u32,
        n100: u32,
        n50: u32,
        misses: u32,
    },
    Taiko {
        n300: u32,
        n100: u32,
        misses: u32,
    },
    Catch {
        fruits: u32,
        droplets: u32,
        tiny_droplets: u32,
        tiny_droplet_misses: u32,
        misses: u32,
    },
    Mania {
        n320: u32,
        n300: u32,
        n200: u32,
        n100: u32,
        n50: u32,
        misses: u32,
    },
}

#[derive(Clone, Copy, Debug)]
enum ScoreInputMode {
    Simple,
    Detailed,
}

impl Display for ScoreInputMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Simple => write!(f, "Simple"),
            Self::Detailed => write!(f, "Detailed"),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();

    let client_id = read_client_id()?;
    let client_secret = read_client_secret()?;

    let osu = Osu::new(client_id, client_secret)
        .await
        .context("failed to create osu! api v2 client")?;

    let username = Input::new("osu! username or user id")
        .placeholder("e.g. peppy or 33138610")
        .prompt("User: ")
        .run()
        .context("failed to read username")?;

    let (api_mode, pp_mode) = read_mode()?;

    let map_id_raw = Input::new("Beatmap ID")
        .placeholder("numeric id, e.g. 3897329")
        .prompt("Beatmap ID: ")
        .run()
        .context("failed to read beatmap id")?;

    let map_id: u32 = map_id_raw
        .trim()
        .parse()
        .context("beatmap id must be an integer")?;

    let mod_bits = read_mods_for_mode(api_mode)?;

    let score_input_mode = read_score_input_mode();

    let (accuracy, combo_opt, counts_opt) = match score_input_mode {
        ScoreInputMode::Detailed => read_detailed_judgements(api_mode)?,
        ScoreInputMode::Simple => read_simple_score()?,
    };

    let map_bytes = download_osu_file(map_id)
        .await
        .with_context(|| format!("failed to download .osu for beatmap {map_id}"))?;

    let map = PpBeatmap::from_bytes(&map_bytes).context("failed to parse .osu file")?;

    if let Err(suspicion) = map.check_suspicion() {
        eyre::bail!("beatmap is suspicious: {suspicion:?}");
    }

    let mut perf = Performance::new(&map)
        .mods(mod_bits)
        .mode_or_ignore(pp_mode);

    if let Some(c) = combo_opt {
        perf = perf.combo(c);
    }

    if let Some(detailed) = counts_opt {
        perf = apply_detailed_judgements(perf, detailed);
    } else if let Some((acc, misses)) = accuracy {
        perf = perf.accuracy(acc).misses(misses);
    }

    let perf_attrs = perf.calculate();
    let new_play_pp = perf_attrs.pp();

    println!();
    println!("Hypothetical play PP: {:.2}pp", new_play_pp);

    let current_scores = fetch_user_best_scores(&osu, username.trim(), api_mode).await?;

    let mut current_pps: Vec<f64> = current_scores
        .iter()
        .filter_map(|s| s.pp)
        .map(|pp| pp as f64)
        .collect();

    current_pps.sort_by(|a, b| b.partial_cmp(a).unwrap());
    let old_total_pp = weighted_total_pp(&current_pps);

    current_pps.push(new_play_pp);
    current_pps.sort_by(|a, b| b.partial_cmp(a).unwrap());
    let new_total_pp = weighted_total_pp(&current_pps);
    let gain = new_total_pp - old_total_pp;

    println!();
    println!("Approx. old total PP (recomputed): {:.2}pp", old_total_pp);
    println!("Approx. new total PP:             {:.2}pp", new_total_pp);
    println!("Approx. PP gain from this play:   {:+.2}pp", gain);

    println!();
    println!("Notes:");
    println!("- Supported modes: osu, taiko, catch, mania.");
    println!("- Mods list mirrors osu!lazer's modifiers per mode.");
    println!("- Lazer‑only / fun mods are shown but do not affect PP here.");
    println!("- Uses classic 0.95^i weighting on your top 100 plays.");
    println!("- Ignores bonus‑PP components.");

    Ok(())
}

fn read_client_id() -> Result<u64> {
    if let Ok(id) = env::var("OSU_CLIENT_ID") {
        return id
            .trim()
            .parse()
            .context("OSU_CLIENT_ID must be an integer client id");
    }

    let raw = Input::new("osu! OAuth client id")
        .placeholder("numeric client id")
        .prompt("Client ID: ")
        .run()
        .context("failed to read client id")?;

    raw.trim().parse().context("client id must be an integer")
}

fn read_client_secret() -> Result<String> {
    if let Ok(secret) = env::var("OSU_CLIENT_SECRET") {
        return Ok(secret);
    }

    let secret = Input::new("osu! OAuth client secret")
        .placeholder("will not be echoed")
        .prompt("Client secret: ")
        .password(true)
        .run()
        .context("failed to read client secret")?;

    Ok(secret)
}

struct GM(GameMode, PpGameMode);

impl From<(GameMode, PpGameMode)> for GM {
    fn from(value: (GameMode, PpGameMode)) -> Self {
        Self(value.0, value.1)
    }
}

impl Display for GM {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            GameMode::Osu => write!(f, "osu!standard"),
            GameMode::Taiko => write!(f, "osu!taiko"),
            GameMode::Catch => write!(f, "osu!catch"),
            GameMode::Mania => write!(f, "osu!mania"),
        }
    }
}

fn read_mode() -> Result<(GameMode, PpGameMode)> {
    let select = Select::new("Game mode")
        .description("Use ↑/↓ and Enter. ESC to cancel.")
        .option(
            DemandOption::new(GM::from((GameMode::Osu, PpGameMode::Osu)))
                .label("osu!standard")
                .description("Circles / sliders / spinners"),
        )
        .option(
            DemandOption::new(GM::from((GameMode::Taiko, PpGameMode::Taiko)))
                .label("osu!taiko")
                .description("Drum rolls"),
        )
        .option(
            DemandOption::new(GM::from((GameMode::Catch, PpGameMode::Catch)))
                .label("osu!catch")
                .description("Catching fruits"),
        )
        .option(
            DemandOption::new(GM::from((GameMode::Mania, PpGameMode::Mania)))
                .label("osu!mania")
                .description("Key‑based"),
        );

    let selection = select
        .run()
        .context("Failed to read gamemode from selection")?;
    let (api_mode, pp_mode) = (selection.0, selection.1);

    Ok((api_mode, pp_mode))
}

fn read_score_input_mode() -> ScoreInputMode {
    let select = Select::new("Score input mode")
        .description("Choose how to describe the play")
        .option(
            DemandOption::new(ScoreInputMode::Simple)
                .label("Simple")
                .description("Accuracy + combo + misses"),
        )
        .option(
            DemandOption::new(ScoreInputMode::Detailed)
                .label("Detailed")
                .description("Enter exact judgement counts"),
        );

    select.run().unwrap_or(ScoreInputMode::Simple)
}

fn read_u32(label: &str, placeholder: &str) -> Result<u32> {
    let raw = Input::new(label)
        .placeholder(placeholder)
        .prompt(&format!("{label}: "))
        .run()
        .with_context(|| format!("failed to read {label}"))?;

    raw.trim()
        .parse()
        .with_context(|| format!("{label} must be an unsigned integer"))
}

fn read_optional_u32(label: &str, placeholder: &str) -> Result<Option<u32>> {
    let raw = Input::new(label)
        .placeholder(placeholder)
        .prompt(&format!("{label}: "))
        .run()
        .with_context(|| format!("failed to read {label}"))?;

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        let v = trimmed
            .parse()
            .with_context(|| format!("{label} must be an unsigned integer"))?;
        Ok(Some(v))
    }
}

type AccuracyAndMisses = Option<(f64, u32)>;

fn read_simple_score() -> Result<(AccuracyAndMisses, Option<u32>, Option<DetailedJudgements>)> {
    let acc_raw = Input::new("Accuracy in %")
        .placeholder("e.g. 98.75")
        .prompt("Accuracy: ")
        .run()
        .context("failed to read accuracy")?;

    let accuracy = acc_raw
        .trim()
        .parse::<f64>()
        .context("accuracy must be a floating number like 98.5")?;

    let misses = read_u32("Number of misses", "usually 0 for FC")?;
    let combo = read_optional_u32(
        "Combo (optional)",
        "leave empty for full combo assumed by rosu-pp",
    )?;

    Ok((Some((accuracy, misses)), combo, None))
}

fn read_detailed_judgements(
    mode: GameMode,
) -> Result<(AccuracyAndMisses, Option<u32>, Option<DetailedJudgements>)> {
    match mode {
        GameMode::Osu => {
            let n300 = read_u32("Number of 300s", "e.g. 1000")?;
            let n100 = read_u32("Number of 100s", "e.g. 10")?;
            let n50 = read_u32("Number of 50s", "e.g. 0")?;
            let misses = read_u32("Number of misses", "e.g. 1")?;
            let combo = read_optional_u32(
                "Combo (optional)",
                "leave empty for full combo assumed by rosu-pp",
            )?;

            Ok((
                None,
                combo,
                Some(DetailedJudgements::Osu {
                    n300,
                    n100,
                    n50,
                    misses,
                }),
            ))
        }
        GameMode::Taiko => {
            let n300 = read_u32("Number of GREATs (300)", "e.g. 1000")?;
            let n100 = read_u32("Number of GOODs (100)", "e.g. 10")?;
            let misses = read_u32("Number of misses", "e.g. 1")?;
            let combo = read_optional_u32(
                "Combo (optional)",
                "leave empty for full combo assumed by rosu-pp",
            )?;

            Ok((
                None,
                combo,
                Some(DetailedJudgements::Taiko { n300, n100, misses }),
            ))
        }
        GameMode::Catch => {
            println!();
            println!("osu!catch detailed input:");
            println!("- Fruits = large objects (300s)");
            println!("- Droplets = big slider droplets");
            println!("- Tiny droplets = small droplets actually caught");
            println!("- Tiny droplet misses = missed tiny droplets");

            let fruits = read_u32("Fruits caught", "e.g. 500")?;
            let droplets = read_u32("Droplets caught", "e.g. 100")?;
            let tiny_droplets = read_u32("Tiny droplets caught", "e.g. 50")?;
            let tiny_droplet_misses = read_u32("Tiny droplet misses", "e.g. 0 (usually small)")?;
            let misses = read_u32("Fruit+droplet misses", "e.g. 0")?;
            let combo = read_optional_u32(
                "Combo (optional)",
                "leave empty for full combo assumed by rosu-pp",
            )?;

            Ok((
                None,
                combo,
                Some(DetailedJudgements::Catch {
                    fruits,
                    droplets,
                    tiny_droplets,
                    tiny_droplet_misses,
                    misses,
                }),
            ))
        }
        GameMode::Mania => {
            println!();
            println!("osu!mania detailed input:");
            println!("- 320 = MAX / rainbow 300 (geki)");
            println!("- 300 = normal 300");
            println!("- 200 = katu");
            println!("- 100 / 50 / miss as usual");

            let n320 = read_u32("Number of 320s (MAX)", "e.g. 1000")?;
            let n300 = read_u32("Number of 300s", "e.g. 100")?;
            let n200 = read_u32("Number of 200s", "e.g. 10")?;
            let n100 = read_u32("Number of 100s", "e.g. 0")?;
            let n50 = read_u32("Number of 50s", "e.g. 0")?;
            let misses = read_u32("Number of misses", "e.g. 1")?;
            let combo = read_optional_u32(
                "Combo (optional)",
                "leave empty for full combo assumed by rosu-pp",
            )?;

            Ok((
                None,
                combo,
                Some(DetailedJudgements::Mania {
                    n320,
                    n300,
                    n200,
                    n100,
                    n50,
                    misses,
                }),
            ))
        }
    }
}

struct ModOptionDef {
    acronym: &'static str,
    bits: u32,
    description: &'static str,
    modes: &'static [GameMode],
}

impl Display for ModOptionDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let out_str = format!(
            "Acronym: {}\n
Bits: {}\n
Description: {}\n
Modes: {}
            ",
            self.acronym,
            self.bits,
            self.description,
            self.modes
                .iter()
                .map(|a| a.as_str())
                .collect::<Vec<_>>()
                .join(",")
        );

        write!(f, "{}", out_str)
    }
}

const fn b(bit: u32) -> u32 {
    1 << bit
}

const MODS_LAZER: &[ModOptionDef] = &[
    ModOptionDef {
        acronym: "EZ",
        bits: b(1),
        description: "Easy",
        modes: &[
            GameMode::Osu,
            GameMode::Taiko,
            GameMode::Catch,
            GameMode::Mania,
        ],
    },
    ModOptionDef {
        acronym: "NF",
        bits: b(0),
        description: "No Fail",
        modes: &[
            GameMode::Osu,
            GameMode::Taiko,
            GameMode::Catch,
            GameMode::Mania,
        ],
    },
    ModOptionDef {
        acronym: "HT",
        bits: b(8),
        description: "Half Time",
        modes: &[
            GameMode::Osu,
            GameMode::Taiko,
            GameMode::Catch,
            GameMode::Mania,
        ],
    },
    ModOptionDef {
        acronym: "DC",
        bits: 0,
        description: "Daycore (lazer only, no PP effect here)",
        modes: &[
            GameMode::Osu,
            GameMode::Taiko,
            GameMode::Catch,
            GameMode::Mania,
        ],
    },
    ModOptionDef {
        acronym: "NR",
        bits: 0,
        description: "No Release (mania only, no PP effect here)",
        modes: &[GameMode::Mania],
    },
    ModOptionDef {
        acronym: "HR",
        bits: b(4),
        description: "Hard Rock",
        modes: &[
            GameMode::Osu,
            GameMode::Taiko,
            GameMode::Catch,
            GameMode::Mania,
        ],
    },
    ModOptionDef {
        acronym: "SD",
        bits: b(5),
        description: "Sudden Death",
        modes: &[
            GameMode::Osu,
            GameMode::Taiko,
            GameMode::Catch,
            GameMode::Mania,
        ],
    },
    ModOptionDef {
        acronym: "PF",
        bits: b(5) | b(14),
        description: "Perfect (full combo SD)",
        modes: &[
            GameMode::Osu,
            GameMode::Taiko,
            GameMode::Catch,
            GameMode::Mania,
        ],
    },
    ModOptionDef {
        acronym: "DT",
        bits: b(6),
        description: "Double Time",
        modes: &[
            GameMode::Osu,
            GameMode::Taiko,
            GameMode::Catch,
            GameMode::Mania,
        ],
    },
    ModOptionDef {
        acronym: "NC",
        bits: b(6) | b(9),
        description: "Nightcore (DT variant)",
        modes: &[
            GameMode::Osu,
            GameMode::Taiko,
            GameMode::Catch,
            GameMode::Mania,
        ],
    },
    ModOptionDef {
        acronym: "HD",
        bits: b(3),
        description: "Hidden",
        modes: &[
            GameMode::Osu,
            GameMode::Taiko,
            GameMode::Catch,
            GameMode::Mania,
        ],
    },
    ModOptionDef {
        acronym: "FI",
        bits: 0,
        description: "Fade In (mania only in stable)",
        modes: &[GameMode::Mania],
    },
    ModOptionDef {
        acronym: "CO",
        bits: 0,
        description: "Cover (lazer only, no PP effect here)",
        modes: &[
            GameMode::Osu,
            GameMode::Taiko,
            GameMode::Catch,
            GameMode::Mania,
        ],
    },
    ModOptionDef {
        acronym: "FL",
        bits: b(10),
        description: "Flashlight",
        modes: &[GameMode::Osu, GameMode::Catch, GameMode::Mania],
    },
    ModOptionDef {
        acronym: "BL",
        bits: 0,
        description: "Blinds (lazer fun mod, no PP effect here)",
        modes: &[GameMode::Osu, GameMode::Catch, GameMode::Mania],
    },
    ModOptionDef {
        acronym: "ST",
        bits: 0,
        description: "Strict Tracking (taiko only, no PP effect here)",
        modes: &[GameMode::Taiko],
    },
    ModOptionDef {
        acronym: "AC",
        bits: 0,
        description: "Accuracy Challenge (lazer only, no PP effect here)",
        modes: &[
            GameMode::Osu,
            GameMode::Taiko,
            GameMode::Catch,
            GameMode::Mania,
        ],
    },
    ModOptionDef {
        acronym: "AT",
        bits: b(7),
        description: "Autoplay (no PP)",
        modes: &[
            GameMode::Osu,
            GameMode::Taiko,
            GameMode::Catch,
            GameMode::Mania,
        ],
    },
    ModOptionDef {
        acronym: "AP",
        bits: b(9),
        description: "AutoPilot (osu!, no PP)",
        modes: &[GameMode::Osu],
    },
    ModOptionDef {
        acronym: "CN",
        bits: 0,
        description: "Cinema (no PP)",
        modes: &[GameMode::Osu, GameMode::Catch],
    },
    ModOptionDef {
        acronym: "RL",
        bits: 0,
        description: "Relax (no PP)",
        modes: &[GameMode::Osu, GameMode::Catch],
    },
    ModOptionDef {
        acronym: "RX",
        bits: 0,
        description: "Classic Relax acronym (no PP)",
        modes: &[GameMode::Osu, GameMode::Catch],
    },
    ModOptionDef {
        acronym: "TD",
        bits: 0,
        description: "Target Practice / Touch Device (no PP)",
        modes: &[GameMode::Osu],
    },
    ModOptionDef {
        acronym: "SO",
        bits: b(12),
        description: "Spun Out (osu! only)",
        modes: &[GameMode::Osu],
    },
    ModOptionDef {
        acronym: "DA",
        bits: 0,
        description: "Difficulty Adjust (lazer only, no PP here)",
        modes: &[
            GameMode::Osu,
            GameMode::Taiko,
            GameMode::Catch,
            GameMode::Mania,
        ],
    },
    ModOptionDef {
        acronym: "TC",
        bits: 0,
        description: "Traceable (lazer only)",
        modes: &[GameMode::Osu],
    },
    ModOptionDef {
        acronym: "WI",
        bits: 0,
        description: "Wiggle (lazer only)",
        modes: &[GameMode::Osu],
    },
    ModOptionDef {
        acronym: "CL",
        bits: 0,
        description: "Classic (lazer: emulate stable quirks)",
        modes: &[GameMode::Osu, GameMode::Taiko],
    },
    ModOptionDef {
        acronym: "RD",
        bits: 0,
        description: "Random (mania only, no PP)",
        modes: &[GameMode::Mania],
    },
    ModOptionDef {
        acronym: "MR",
        bits: 0,
        description: "Mirror (mania only, no PP)",
        modes: &[GameMode::Mania],
    },
    ModOptionDef {
        acronym: "ATC",
        bits: 0,
        description: "Adaptive Speed / Challenge (lazer system, no PP)",
        modes: &[
            GameMode::Osu,
            GameMode::Taiko,
            GameMode::Catch,
            GameMode::Mania,
        ],
    },
    ModOptionDef {
        acronym: "1K",
        bits: 0,
        description: "1 key (mania only, no legacy bit)",
        modes: &[GameMode::Mania],
    },
    ModOptionDef {
        acronym: "2K",
        bits: 0,
        description: "2 keys",
        modes: &[GameMode::Mania],
    },
    ModOptionDef {
        acronym: "3K",
        bits: 0,
        description: "3 keys",
        modes: &[GameMode::Mania],
    },
    ModOptionDef {
        acronym: "4K",
        bits: b(15),
        description: "4 keys",
        modes: &[GameMode::Mania],
    },
    ModOptionDef {
        acronym: "5K",
        bits: b(16),
        description: "5 keys",
        modes: &[GameMode::Mania],
    },
    ModOptionDef {
        acronym: "6K",
        bits: b(17),
        description: "6 keys",
        modes: &[GameMode::Mania],
    },
    ModOptionDef {
        acronym: "7K",
        bits: b(18),
        description: "7 keys",
        modes: &[GameMode::Mania],
    },
    ModOptionDef {
        acronym: "8K",
        bits: b(19),
        description: "8 keys",
        modes: &[GameMode::Mania],
    },
    ModOptionDef {
        acronym: "9K",
        bits: b(24),
        description: "9 keys",
        modes: &[GameMode::Mania],
    },
];

fn read_mods_for_mode(mode: GameMode) -> Result<u32> {
    let mut ms = MultiSelect::new("Mods")
        .description(
            "Space = toggle, Enter = confirm. Empty = NoMod.\n\
                      Some lazer‑only mods are shown but will not affect PP.",
        )
        .min(0)
        .filterable(true);

    for m in MODS_LAZER.iter().filter(|m| m.modes.contains(&mode)) {
        ms = ms.option(
            DemandOption::new(m)
                .label(m.acronym)
                .description(m.description),
        );
    }

    let selected = ms.run().context("failed to run mods multiselect")?;

    let mut bits = 0u32;
    for m in selected {
        bits |= m.bits;
    }

    Ok(bits)
}

fn apply_detailed_judgements(
    perf: Performance<'_>,
    detailed: DetailedJudgements,
) -> Performance<'_> {
    match detailed {
        DetailedJudgements::Osu {
            n300,
            n100,
            n50,
            misses,
        } => perf.n300(n300).n100(n100).n50(n50).misses(misses),

        DetailedJudgements::Taiko { n300, n100, misses } => {
            perf.n300(n300).n100(n100).misses(misses)
        }

        DetailedJudgements::Catch {
            fruits,
            droplets,
            tiny_droplets,
            tiny_droplet_misses,
            misses,
        } => perf
            .n300(fruits)
            .large_tick_hits(droplets)
            .small_tick_hits(tiny_droplets)
            .n_katu(tiny_droplet_misses)
            .misses(misses),

        DetailedJudgements::Mania {
            n320,
            n300,
            n200,
            n100,
            n50,
            misses,
        } => perf
            .n_geki(n320)
            .n300(n300)
            .n_katu(n200)
            .n100(n100)
            .n50(n50)
            .misses(misses),
    }
}

async fn fetch_user_best_scores(osu: &Osu, user_input: &str, mode: GameMode) -> Result<Vec<Score>> {
    let trimmed = user_input.trim();

    let builder = if let Ok(id) = trimmed.parse::<u32>() {
        osu.user_scores(id)
    } else {
        osu.user_scores(trimmed)
    };

    let scores = builder
        .mode(mode)
        .best()
        .limit(100)
        .await
        .context("failed to fetch user top scores")?;

    Ok(scores)
}

async fn download_osu_file(map_id: u32) -> Result<Vec<u8>> {
    let url = format!("https://osu.ppy.sh/osu/{map_id}");

    let bytes = reqwest::get(&url)
        .await
        .with_context(|| format!("GET {url} failed"))?
        .error_for_status()
        .with_context(|| format!("{url} returned non-success status"))?
        .bytes()
        .await
        .context("failed to read response body")?;

    Ok(bytes.to_vec())
}

fn weighted_total_pp(pps: &[f64]) -> f64 {
    pps.iter()
        .take(100)
        .enumerate()
        .map(|(i, pp)| pp * 0.95_f64.powi(i as i32))
        .sum()
}
