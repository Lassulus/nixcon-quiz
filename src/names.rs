//! Player names are handed out by the server, never chosen, so the
//! leaderboard on the big screen stays safe for work.

use rand::seq::IndexedRandom;
use std::collections::HashSet;

const ADJECTIVES: &[&str] = &[
    "Lazy",
    "Pure",
    "Hermetic",
    "Reproducible",
    "Declarative",
    "Immutable",
    "Atomic",
    "Functional",
    "Sandboxed",
    "Pinned",
    "Cached",
    "Evaluated",
    "Substituted",
    "Signed",
    "Fixed-Output",
    "Content-Addressed",
    "Impure",
    "Recursive",
    "Unfree",
    "Deduplicated",
    "Garbage-Collected",
    "Cross-Compiled",
    "Bootstrapped",
    "Overridden",
];

const NOUNS: &[&str] = &[
    "Derivation",
    "Flake",
    "Closure",
    "Overlay",
    "Thunk",
    "Snowflake",
    "Hydra",
    "Channel",
    "Module",
    "Store Path",
    "Lambda",
    "Attrset",
    "Fixpoint",
    "Substituter",
    "Generation",
    "Profile",
    "Builder",
    "Callpackage",
    "Nixling",
    "Hash",
    "Rebuild",
    "Shell",
    "Sandbox",
    "Output",
];

/// A random name not in `taken`. Once the plain combinations run out a
/// number is appended.
pub fn generate(taken: &HashSet<String>) -> String {
    let mut rng = rand::rng();
    let mut pick = || {
        format!(
            "{} {}",
            ADJECTIVES.choose(&mut rng).unwrap(),
            NOUNS.choose(&mut rng).unwrap()
        )
    };
    for _ in 0..32 {
        let name = pick();
        if !taken.contains(&name) {
            return name;
        }
    }
    let base = pick();
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|name| !taken.contains(name))
        .unwrap()
}
