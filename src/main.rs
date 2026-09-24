mod game;
mod html;
mod names;
mod quiz;
mod web;

use std::{path::PathBuf, process::ExitCode};

const USAGE: &str = "\
usage: nixcon-quiz --questions DIR [options]
       nixcon-quiz --check DIR

  --questions DIR            directory with one TOML file per question
  --check DIR                validate the question files and exit
  --listen ADDR:PORT         address to serve on (default 127.0.0.1:8080)
  --public-url URL           join address shown on the /spectate screen
                             (default: the host the screen was loaded from)
  --title TEXT               page title (default \"NixCon Quiz\")
  --question-seconds N       time to answer each question (default 20)
  --reveal-seconds N         how long the right answer is shown (default 8)
  --round-questions N        questions per round, then leaderboard and points reset (default 10)
  --leaderboard-seconds N    how long the leaderboard is shown (default 30)";

fn positive<T: std::str::FromStr + Default + PartialOrd>(
    arg: &str,
    value: String,
) -> Result<T, String> {
    match value.parse() {
        Ok(n) if n > T::default() => Ok(n),
        _ => Err(format!("{arg} needs a positive number, got {value:?}")),
    }
}

fn main() -> ExitCode {
    let mut questions = None;
    let mut check = None;
    let mut listen = String::from("127.0.0.1:8080");
    let mut public_url = None;
    let mut settings = game::Settings::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        let parsed = match arg.as_str() {
            "--questions" => value().map(|v| questions = Some(PathBuf::from(v))),
            "--check" => value().map(|v| check = Some(PathBuf::from(v))),
            "--listen" => value().map(|v| listen = v),
            "--public-url" => value().map(|v| public_url = Some(v)),
            "--title" => value().map(|v| settings.title = v),
            "--question-seconds" => value()
                .and_then(|v| positive(&arg, v))
                .map(|n| settings.question_seconds = n),
            "--reveal-seconds" => value()
                .and_then(|v| positive(&arg, v))
                .map(|n| settings.reveal_seconds = n),
            "--round-questions" => value()
                .and_then(|v| positive(&arg, v))
                .map(|n| settings.round_questions = n),
            "--leaderboard-seconds" => value()
                .and_then(|v| positive(&arg, v))
                .map(|n| settings.leaderboard_seconds = n),
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            _ => Err(format!("unknown argument {arg:?}")),
        };
        if let Err(e) = parsed {
            eprintln!("{e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    }

    if let Some(dir) = check {
        return match quiz::load_dir(&dir) {
            Ok(q) => {
                println!("{}: {} questions, ok", dir.display(), q.len());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        };
    }

    let Some(dir) = questions else {
        eprintln!("--questions is required\n\n{USAGE}");
        return ExitCode::from(2);
    };
    let questions = match quiz::load_dir(&dir) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let result = runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(&listen).await?;
        eprintln!(
            "serving {} questions from {} on http://{}",
            questions.len(),
            dir.display(),
            listener.local_addr()?
        );
        web::serve(
            listener,
            game::Game::new(settings, questions, web::now_ms()),
            public_url,
        )
        .await
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{listen}: {e}");
            ExitCode::FAILURE
        }
    }
}
