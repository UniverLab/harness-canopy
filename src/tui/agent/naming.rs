/// Creative session names assigned when the user doesn't provide one (interactive agents).
const RANDOM_NAMES: &[&str] = &[
    "liquidambar",
    "wollemia",
    "metasequoia",
    "paulownia",
    "liriodendron",
    "cryptomeria",
    "cunninghamia",
    "nothofagus",
    "podocarpus",
    "fitzroya",
    "cephalotaxus",
    "taiwania",
    "sciadopitys",
    "toona",
    "cedrus",
    "sequoia",
    "juniperus",
    "stereum",
    "larix",
    "carpinus",
    "castanea",
    "aesculus",
    "juglans",
    "platanus",
    "agaricus",
    "araucaria",
    "zelkova",
    "magnolia",
    "ginkgo",
    "quercus",
    "boletus",
    "morchella",
    "cantharellus",
    "pleurotus",
    "ganoderma",
    "lentinula",
    "hydnum",
    "trametes",
    "russula",
    "lactarius",
    "populus",
    "laricifomes",
    "cordyceps",
    "hericium",
    "laetiporus",
    "armillaria",
    "clavaria",
    "geastrum",
    "lycoperdon",
    "mycena",
    "marasmius",
    "cortinarius",
    "hygrocybe",
    "xylaria",
    "fistulina",
    "grifola",
    "daedalea",
    "clitocybe",
    "inocybe",
    "pholiota",
    "stropharia",
    "suillus",
    "omphalotus",
    "sparassis",
    "calvatia",
    "tilia",
    "betula",
    "salix",
    "fagus",
    "acer",
    "ulmus",
    "fraxinus",
    "taxus",
    "tuber",
    "ramaria",
    "tricholoma",
    "agrocybe",
];

/// Session names for background/scheduled agents (weather/nature terms).
const BACKGROUND_NAMES: &[&str] = &[
    "foehn",
    "mistral",
    "tramontana",
    "galerna",
    "fitoncida",
    "espora",
    "micela",
    "rizoma",
    "lignina",
    "tanino",
    "resina",
    "humus",
];

/// Session names for raw terminal sessions (minerals).
const TERMINAL_NAMES: &[&str] = &[
    "feldspato",
    "cuarzo",
    "olivino",
    "piroxeno",
    "anfíbol",
    "biotita",
    "moscovita",
    "clorita",
    "caolinita",
    "illita",
    "esmectita",
    "vermiculita",
    "haloisita",
    "sepiolita",
    "palygorskita",
    "bario",
    "estroncio",
    "rubidio",
    "vanadio",
    "cobalto",
    "molibdeno",
    "niquel",
    "cesio",
];

/// Pick a name from `names` that isn't already in `existing`.
///
/// First tries each name bare.  On collision appends `-2`, `-3`, …
/// Falls back to a UUID-based ID if every combination is taken.
fn pick_name_from(names: &[&str], existing: &[&str]) -> String {
    use rand::prelude::IndexedRandom;

    // First try: pick a random bare name that isn't in use
    let available: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| !existing.contains(n))
        .collect();
    if let Some(&name) = available.choose(&mut rand::rng()) {
        return name.to_string();
    }

    // Second try: pick a random base name and try <name>-2, <name>-3, …
    if let Some(&base) = names.choose(&mut rand::rng()) {
        for n in 2..=999u32 {
            let candidate = format!("{}-{}", base, n);
            if !existing.contains(&candidate.as_str()) {
                return candidate;
            }
        }
    }

    format!("session-{}", &uuid::Uuid::new_v4().to_string()[..8])
}

/// Pick a random name for interactive agents (trees + fungi).
pub fn pick_random_name(existing: &[&str]) -> String {
    pick_name_from(RANDOM_NAMES, existing)
}

/// Pick a name for terminal sessions (minerals).
pub fn pick_terminal_name(existing: &[&str]) -> String {
    pick_name_from(TERMINAL_NAMES, existing)
}

/// Pick a name for background/scheduled agents (weather/nature terms).
#[allow(dead_code)]
pub fn pick_background_name(existing: &[&str]) -> String {
    pick_name_from(BACKGROUND_NAMES, existing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const DENYLIST: &[&str] = &[
        "phallus",
        "psilocybe",
        "coprinus",
        "amanita",
        "death",
        "dead",
        "kill",
        "poison",
        "toxic",
        "curse",
        "hate",
        "doom",
        "plague",
    ];

    #[test]
    fn session_names_are_safe_and_unique() {
        for list in [RANDOM_NAMES, BACKGROUND_NAMES, TERMINAL_NAMES] {
            for name in list {
                assert!(!DENYLIST.contains(name), "denied name present: {name}");
            }
            let unique: HashSet<&&str> = list.iter().collect();
            assert_eq!(unique.len(), list.len(), "duplicate entry in list");
        }
        let mut all = Vec::new();
        for list in [RANDOM_NAMES, BACKGROUND_NAMES, TERMINAL_NAMES] {
            all.extend(list.iter());
        }
        let unique: HashSet<&&str> = all.iter().copied().collect();
        assert_eq!(unique.len(), all.len(), "duplicate across lists");

        assert_eq!(pick_name_from(&["tilia"], &["tilia"]), "tilia-2");
        assert_eq!(pick_name_from(&["tilia"], &["tilia", "tilia-2"]), "tilia-3");
    }
}
