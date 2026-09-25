//! Server-rendered HTML. The page is rendered once on load; afterwards the
//! server pushes a fresh `game` fragment over Server-Sent Events whenever the
//! phase changes and htmx swaps it in. Answers are a plain form of radio
//! buttons that htmx posts on every change.
//!
//! The spectator screen for the livestream is rendered from the same views,
//! seen by nobody in particular, in a 16:9 layout with a QR code to join and,
//! when there are any, break slides filling the stage with the game beside.

use crate::{
    game::{BASE_POINTS, PhaseView, SPEED_POINTS, Standing, Tally, View, points},
    slides::Slide,
};
use maud::{DOCTYPE, Markup, html};

pub const HTMX: &str = "/vendor/htmx-2.0.11.min.js";
pub const HTMX_SSE: &str = "/vendor/htmx-ext-sse-2.2.4.min.js";

const LETTERS: [char; 6] = ['A', 'B', 'C', 'D', 'E', 'F'];

/// Where one game lives. The normal quiz and the fast one each have their
/// own pages, streams and answer endpoint.
pub struct Urls {
    pub page: &'static str,
    pub spectate: &'static str,
    pub qr: &'static str,
    pub events: &'static str,
    pub spectate_events: &'static str,
    pub answer: &'static str,
}

/// The whole document, with the current view already in place. `seen` names
/// the state rendered here, for the event stream.
pub fn page(view: &View, urls: &Urls, seen: &str) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            (head(view.title))
            body {
                div #game hx-ext="sse" sse-connect={ (urls.events) "?seen=" (seen) } sse-swap="message" {
                    (game(view, urls))
                }
                p #connection {}
            }
        }
    }
}

/// The livestream screen. `join` is the URL players should open. `slide` is
/// the break slide to show, `None` when the screen has no slides at all.
pub fn spectate_page(
    view: &View,
    tally: Tally,
    urls: &Urls,
    join: &str,
    slide: Option<Option<&Slide>>,
) -> Markup {
    let shown = join
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    html! {
        (DOCTYPE)
        html lang="en" {
            (head(view.title))
            body.spectate {
                div.stage hx-ext="sse" sse-connect=(urls.spectate_events) {
                    div #game sse-swap="message" {
                        (spectate_game(view, tally))
                    }
                    @if let Some(current) = slide {
                        aside #slide sse-swap="slide" { (self::slide(current)) }
                    }
                    aside.join {
                        img.qr src=(urls.qr) alt={ "QR code for " (join) };
                        div.how {
                            p.label { "join the quiz" }
                            p.url { (shown.trim_end_matches('/')) }
                            p.hint { "scan the code or open the address on your phone" }
                            (slop_warning())
                        }
                        ol.steps {
                            li { "everyone gets the same question at the same time" }
                            li { "tap an answer; you can change it until the time runs out" }
                            li { "right answers score, faster ones score more" }
                        }
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
pub fn game(view: &View, urls: &Urls) -> Markup {
    let me = view.me.as_ref().expect("a player's view");
    html! {
        div.view data-now=(view.now) data-phase=(phase_name(&view.phase)) {
            header {
                div.brand {
                    (logo())
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
                    question @ PhaseView::Question { number, of, text, ends, .. } => {
                        (heading(*number, *of, text))
                        (answer_form(urls.answer, question, view.now))
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
                                (result("good", &format!("+{points}"), &format!(
                                    "build succeeded: {BASE_POINTS} for right + {} for speed",
                                    points - BASE_POINTS,
                                )))
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
                (slop_warning())
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
                    (logo())
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

/// The whole site, questions and all, was written with an LLM; say so, in
/// Nix's own words.
fn slop_warning() -> Markup {
    html! {
        span.slop {
            span.warning { "warning:" }
            " this whole site is LLM-slopped, answers included"
        }
    }
}

/// The payload of `slide` events: empty while the slides directory is.
pub fn slide(slide: Option<&Slide>) -> Markup {
    html! {
        @if let Some(slide) = slide {
            img src={ "/slides/" (slide.name) } alt=(slide.alt);
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

/// A QR code as a standalone SVG document, one unit per module plus the
/// quiet zone. The colours are part of the image, not the page's CSS, so
/// extensions like Dark Reader, which recolour the page, can't turn it into
/// something that no longer scans.
pub fn qr_svg(url: &str) -> Option<String> {
    const QUIET: usize = 2;
    let code = qrcode::QrCode::new(url).ok()?;
    let width = code.width();
    let mut d = String::new();
    for (i, colour) in code.to_colors().iter().enumerate() {
        if *colour == qrcode::Color::Dark {
            let (x, y) = (i % width + QUIET, i / width + QUIET);
            d.push_str(&format!("M{x} {y}h1v1h-1z"));
        }
    }
    let size = width + 2 * QUIET;
    let svg = html! {
        svg xmlns="http://www.w3.org/2000/svg" viewBox={ "0 0 " (size) " " (size) } shape-rendering="crispEdges" {
            rect width=(size) height=(size) fill="#fff" {}
            path d=(d) fill="#2f2f2f" {}
        }
    };
    Some(svg.into_string())
}

/// The NixCon 2026 eagle from 2026.nixcon.org.
fn logo() -> Markup {
    html! {
        img.logo src="/vendor/nixcon-2026-icon.svg" alt="";
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

/// A radio per choice, posted to `url`. htmx posts the form on every change;
/// `queue last` keeps requests in order so quick switching can't leave an
/// older pick on the server. The picked state is the radio itself, so
/// nothing is swapped back.
fn answer_form(url: &str, question: &PhaseView, now: u64) -> Markup {
    let PhaseView::Question {
        seq,
        choices,
        started,
        ends,
        answered,
        pick_points,
        ..
    } = *question
    else {
        return html! {};
    };
    html! {
        form #answer hx-post=(url) hx-trigger="change" hx-swap="none"
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
            (worth(pick_points, started, ends, now))
            p.status {
                span.unpicked { "pick an answer" }
                span.picked { "you can change your answer until the time runs out; the speed bonus then counts from the new pick" }
                span.expired { "time's up" }
                span.notice {}
            }
        }
    }
}

/// What a right answer is worth: counting down with the clock until you
/// pick, then fixed at what your pick earns if it's right. clock.js keeps
/// it running with the same formula as `game::points`.
fn worth(pick_points: Option<u64>, started: u64, ends: u64, now: u64) -> Markup {
    let shown = pick_points.unwrap_or_else(|| points(started, ends, now));
    html! {
        p.worth data-started=(started) data-ends=(ends) data-base=(BASE_POINTS)
            data-speed=(SPEED_POINTS) data-picked=[pick_points] {
            span.value { "+" (shown) }
            span.live { "for a right answer now" }
            span.locked { "if your pick is right" }
            span.rule { (BASE_POINTS) " for right + up to " (SPEED_POINTS) " for speed" }
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

    const URLS: super::Urls = super::Urls {
        page: "/",
        spectate: "/spectate",
        qr: "/qr.svg",
        events: "/api/events",
        spectate_events: "/api/events/spectate",
        answer: "/api/answer",
    };

    #[test]
    fn open_question_page_does_not_contain_the_answer() {
        let mut g = game();
        let (id, _) = g.join(None, 1_000_000).unwrap();
        let page = super::page(&g.view(id, 1_000_000), &URLS, "v").into_string();
        assert!(page.contains("Which one?"));
        assert!(!page.contains("secret"), "explanation leaked");
        assert!(!page.contains("correct"), "correct choice marked");

        g.tick(g.deadline());
        let reveal = super::game(&g.view(id, g.deadline()), &URLS).into_string();
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
        let page = super::page(&g.view(id, 1_000_002), &URLS, "v").into_string();
        assert!(page.contains(r#"value="1" checked"#), "{page}");
        assert!(!page.contains(r#"value="0" checked"#));
    }
}
