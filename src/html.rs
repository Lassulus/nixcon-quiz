//! Server-rendered HTML. The page is rendered once on load; afterwards the
//! server pushes a fresh `game` fragment over Server-Sent Events whenever the
//! phase changes and htmx swaps it in. Answers are a plain form of radio
//! buttons that htmx posts on every change.
//!
//! The spectator screen for the livestream is rendered from the same views,
//! seen by nobody in particular, in a 16:9 layout with a QR code to join.

use crate::game::{PhaseView, Standing, Tally, View};
use maud::{DOCTYPE, Markup, html};

pub const HTMX: &str = "/vendor/htmx-2.0.11.min.js";
pub const HTMX_SSE: &str = "/vendor/htmx-ext-sse-2.2.4.min.js";

const LETTERS: [char; 6] = ['A', 'B', 'C', 'D', 'E', 'F'];

/// The whole document, with the current view already in place. `events` is
/// the event stream URL, which names the state rendered here.
pub fn page(view: &View, events: &str) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            (head(view.title))
            body {
                div #game hx-ext="sse" sse-connect=(events) sse-swap="message" {
                    (game(view))
                }
                p #connection {}
            }
        }
    }
}

/// The livestream screen. `join` is the URL players should open.
pub fn spectate_page(view: &View, tally: Tally, join: &str) -> Markup {
    let shown = join
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    html! {
        (DOCTYPE)
        html lang="en" {
            (head(view.title))
            body.spectate {
                div.stage {
                    div #game hx-ext="sse" sse-connect="/api/events/spectate" sse-swap="message" {
                        (spectate_game(view, tally))
                    }
                    aside.join {
                        p.label { "join at" }
                        p.url { (shown.trim_end_matches('/')) }
                        (qr(join))
                        p.hint { "scan to play along" }
                    }
                }
                p #connection {}
            }
        }
    }
}

fn head(title: &str) -> Markup {
    html! {
        head {
            meta charset="utf-8";
            meta name="viewport" content="width=device-width, initial-scale=1";
            meta name="color-scheme" content="light";
            // No inline <style> from htmx, no eval, and no carrying the
            // timer bar's style attribute across swaps: the CSP forbids
            // inline styles and eval.
            meta name="htmx-config"
                content=r#"{"includeIndicatorStyles":false,"allowEval":false,"attributesToSettle":["class"]}"#;
            title { (title) }
            link rel="stylesheet" href="/style.css";
            script src=(HTMX) defer {}
            script src=(HTMX_SSE) defer {}
            script src="/clock.js" defer {}
        }
    }
}

fn phase_name(phase: &PhaseView) -> &'static str {
    match phase {
        PhaseView::Question { .. } => "question",
        PhaseView::Reveal { .. } => "reveal",
        PhaseView::Leaderboard { .. } => "leaderboard",
    }
}

fn timer(phase: &PhaseView) -> Markup {
    html! {
        div #timer aria-hidden="true" {
            @if let PhaseView::Question { started, ends, .. } = phase {
                div #bar data-started=(started) data-ends=(ends) {}
            } @else {
                div #bar {}
            }
        }
    }
}

/// Everything that changes during the game; the payload of every event.
pub fn game(view: &View) -> Markup {
    let me = view.me.as_ref().expect("a player's view");
    html! {
        div.view data-now=(view.now) data-phase=(phase_name(&view.phase)) {
            header {
                div.brand {
                    (snowflake())
                    h1 { (view.title) }
                }
                div #me {
                    span #name { (me.name) }
                    span #standing {
                        (me.score) " pts"
                        @if let Some(rank) = me.rank { " · #" (rank) }
                    }
                }
            }
            (timer(&view.phase))
            main {
                @match &view.phase {
                    PhaseView::Question { seq, number, of, text, choices, ends, answered, .. } => {
                        (heading(*number, *of, text))
                        (answer_form(*seq, choices, *answered, *ends, view.now))
                        (countdown(*ends, view.now, "{}s", true))
                    }
                    PhaseView::Reveal {
                        number, of, text, choices, ends, correct, counts, explanation,
                        answered, gained,
                    } => {
                        (heading(*number, *of, text))
                        @match (answered, gained) {
                            (None, _) => (result("none", "no answer", "error: evaluation timed out")),
                            (Some(_), Some(points)) => {
                                (result("good", &format!("+{points}"), "build succeeded"))
                            }
                            (Some(_), None) => {
                                (result("bad", "wrong", "error: builder for 'answer.drv' failed"))
                            }
                        }
                        (revealed(choices, *correct, counts, *answered))
                        p.explanation { (explanation) }
                        (countdown(*ends, view.now, "next question in {}s", false))
                    }
                    PhaseView::Leaderboard { ends, top, players } => {
                        p.label { "/nix/var/nix/profiles/leaderboard" }
                        h2.question { "Round over" }
                        (standings(top, Some(me.id)))
                        p.status {
                            @if let Some(rank) = me.rank {
                                "You finished #" (rank) " of " (players) " with "
                                (me.score) " points"
                            } @else {
                                "No points for you this round. Next one!"
                            }
                        }
                        (countdown(*ends, view.now, "next round in {}s", false))
                    }
                }
            }
            footer {
                span { (view.online) " online" }
            }
        }
    }
}

/// The livestream's picture of the game: like a player's, without the
/// player, bigger, and with a live count of answers.
pub fn spectate_game(view: &View, tally: Tally) -> Markup {
    html! {
        div.view data-now=(view.now) data-phase=(phase_name(&view.phase)) {
            header {
                div.brand {
                    (snowflake())
                    h1 { (view.title) }
                }
                span #tally sse-swap="tally" { (tally_text(tally, &view.phase)) }
            }
            (timer(&view.phase))
            main {
                @match &view.phase {
                    PhaseView::Question { number, of, text, choices, ends, .. } => {
                        (heading(*number, *of, text))
                        div.choices {
                            @for (i, text) in choices.iter().enumerate() {
                                div.choice {
                                    span.letter { (LETTERS[i]) }
                                    span.text { (text) }
                                }
                            }
                        }
                        (countdown(*ends, view.now, "{}s", true))
                    }
                    PhaseView::Reveal { number, of, text, choices, ends, correct, counts, explanation, .. } => {
                        (heading(*number, *of, text))
                        (revealed(choices, *correct, counts, None))
                        p.explanation { (explanation) }
                        (countdown(*ends, view.now, "next question in {}s", false))
                    }
                    PhaseView::Leaderboard { ends, top, .. } => {
                        p.label { "/nix/var/nix/profiles/leaderboard" }
                        h2.question { "Round over" }
                        (standings(top, None))
                        (countdown(*ends, view.now, "next round in {}s", false))
                    }
                }
            }
        }
    }
}

/// The payload of `tally` events; also rendered into every fragment.
pub fn tally_text(tally: Tally, phase: &PhaseView) -> String {
    match phase {
        PhaseView::Question { .. } | PhaseView::Reveal { .. } => {
            format!("{} answered · {} online", tally.answered, tally.online)
        }
        PhaseView::Leaderboard { .. } => format!("{} online", tally.online),
    }
}

fn revealed(choices: &[String], correct: usize, counts: &[u32], answered: Option<usize>) -> Markup {
    let total: u32 = counts.iter().sum();
    html! {
        div.choices {
            @for (i, text) in choices.iter().enumerate() {
                div class={
                    "choice revealed"
                    @if i == correct { " correct" }
                    @else if answered == Some(i) { " wrong" }
                } {
                    progress.share value=(counts[i]) max=(total.max(1)) {}
                    span.letter { (LETTERS[i]) }
                    span.text { (text) }
                    span.count { (counts[i]) }
                }
            }
        }
    }
}

/// `me` is highlighted when it made the list.
fn standings(top: &[Standing], me: Option<u64>) -> Markup {
    html! {
        @if top.is_empty() {
            p.status { "Nobody scored this round." }
        } @else {
            ol.leaderboard {
                @for entry in top {
                    li.me[Some(entry.id) == me] {
                        span.rank { "#" (entry.rank) }
                        span.who { (entry.name) }
                        span.points { (entry.score) }
                    }
                }
            }
        }
    }
}

/// A QR code as an SVG path, one unit per module plus the quiet zone.
fn qr(url: &str) -> Markup {
    const QUIET: usize = 2;
    let Ok(code) = qrcode::QrCode::new(url) else {
        return html! {};
    };
    let width = code.width();
    let mut d = String::new();
    for (i, colour) in code.to_colors().iter().enumerate() {
        if *colour == qrcode::Color::Dark {
            let (x, y) = (i % width + QUIET, i / width + QUIET);
            d.push_str(&format!("M{x} {y}h1v1h-1z"));
        }
    }
    let size = width + 2 * QUIET;
    html! {
        svg.qr viewBox={ "0 0 " (size) " " (size) } shape-rendering="crispEdges" role="img" aria-label={ "QR code for " (url) } {
            rect width=(size) height=(size) {}
            path d=(d) {}
        }
    }
}

/// The NixOS lambda snowflake, from nixos-artwork (CC-BY 4.0), with flat
/// colours instead of gradients.
fn snowflake() -> Markup {
    const LAMBDA: &str = "m -97.76,5.41 122.19683,211.67512 -56.15706,0.5268 -32.6236,-56.8692 \
        -32.85645,56.5653 -27.90237,-0.011 -14.29086,-24.6896 46.81047,-80.4901 -33.22946,-57.8257 z";
    html! {
        svg.snowflake viewBox="-250 -250 500 500" aria-hidden="true" {
            defs { path #lambda d=(LAMBDA) {} }
            @for angle in [60, 180, 300] {
                use href="#lambda" fill="#7ebae4" transform={ "rotate(" (angle) ")" } {}
            }
            @for angle in [0, 120, 240] {
                use href="#lambda" fill="#5277c3" transform={ "rotate(" (angle) ")" } {}
            }
        }
    }
}

/// Decoration: questions are labelled like store paths, with a hash of the
/// question text in Nix's base-32 alphabet.
fn store_path(text: &str, number: u32, of: u32) -> String {
    const NIX32: &[u8; 32] = b"0123456789abcdfghijklmnpqrsvwxyz";
    // FNV-1a
    let mut hash = text.bytes().fold(0xcbf29ce484222325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    let digest: String = (0..8)
        .map(|_| {
            let c = NIX32[(hash & 31) as usize] as char;
            hash >>= 5;
            c
        })
        .collect();
    format!("/nix/store/{digest}-question-{number}-of-{of}")
}

fn heading(number: u32, of: u32, text: &str) -> Markup {
    html! {
        p.label { (store_path(text, number, of)) }
        h2.question { (text) }
    }
}

fn result(class: &str, headline: &str, log: &str) -> Markup {
    html! {
        p class={ "result " (class) } {
            span.headline { (headline) }
            span.log { (log) }
        }
    }
}

/// A radio per choice. htmx posts the form on every change; `queue last`
/// keeps requests in order so quick switching can't leave an older pick on
/// the server. The picked state is the radio itself, so nothing is swapped
/// back.
fn answer_form(
    seq: u64,
    choices: &[String],
    answered: Option<usize>,
    ends: u64,
    now: u64,
) -> Markup {
    html! {
        form #answer hx-post="/api/answer" hx-trigger="change" hx-swap="none"
            hx-sync="this:queue last" data-saved=[answered] {
            input type="hidden" name="seq" value=(seq);
            fieldset.choices data-closes=(ends) disabled[now >= ends] {
                @for (i, text) in choices.iter().enumerate() {
                    label.choice {
                        input type="radio" name="choice" value=(i) checked[answered == Some(i)];
                        span.letter { (LETTERS[i]) }
                        span.text { (text) }
                    }
                }
            }
            p.status {
                span.unpicked { "pick an answer" }
                span.picked { "you can change your answer until the time runs out" }
                span.expired { "time's up" }
                span.notice {}
            }
        }
    }
}

/// Rendered with the seconds left at render time; clock.js keeps it running.
fn countdown(ends: u64, now: u64, label: &str, big: bool) -> Markup {
    let seconds = ends.saturating_sub(now).div_ceil(1000);
    html! {
        p.countdown.big[big] data-ends=(ends) data-label=(label) {
            (label.replace("{}", &seconds.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::game::{Game, Settings};

    fn game() -> Game {
        let question = crate::quiz::parse(
            r#"
            question = "Which one?"
            choices = ["wrong", "right"]
            answer = "right"
            explanation = "Because it is the secret."
            "#,
        )
        .unwrap();
        Game::new(Settings::default(), vec![question], 1_000_000)
    }

    #[test]
    fn open_question_page_does_not_contain_the_answer() {
        let mut g = game();
        let (id, _) = g.join(None, 1_000_000).unwrap();
        let page = super::page(&g.view(id, 1_000_000), "/api/events").into_string();
        assert!(page.contains("Which one?"));
        assert!(!page.contains("secret"), "explanation leaked");
        assert!(!page.contains("correct"), "correct choice marked");

        g.tick(g.deadline());
        let reveal = super::game(&g.view(id, g.deadline())).into_string();
        assert!(reveal.contains("secret"));
        let correct = reveal.find(r#"class="choice revealed correct""#).unwrap();
        let row = reveal[correct..].split("</div>").next().unwrap();
        assert!(row.contains(">right<"), "{row}");
    }

    #[test]
    fn saved_answer_is_checked_when_the_page_is_rendered_again() {
        let mut g = game();
        let (id, _) = g.join(None, 1_000_000).unwrap();
        let seq = match g.view(id, 0).phase {
            crate::game::PhaseView::Question { seq, .. } => seq,
            _ => unreachable!(),
        };
        g.answer(id, seq, 1, 1_000_001).unwrap();
        let page = super::page(&g.view(id, 1_000_002), "/api/events").into_string();
        assert!(page.contains(r#"value="1" checked"#), "{page}");
        assert!(!page.contains(r#"value="0" checked"#));
    }
}
