//! Questions: a directory with one TOML file per question, read once at
//! startup and never sent to clients.
//!
//! ```toml
//! question = "Which language are Nix expressions written in?"
//! choices = ["Nix", "Guile", "YAML", "HCL"]
//! answer = "Nix"
//! ```
//!
//! Only `*.toml` files are questions, so a README can live next to them.

use serde::Deserialize;
use std::{fmt, path::Path};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    question: String,
    choices: Vec<String>,
    /// Must be spelled exactly like one of `choices`.
    answer: String,
    /// Shown with the answer once the question closes.
    explanation: String,
}

#[derive(Debug, Clone)]
pub struct Question {
    pub text: String,
    pub choices: Vec<String>,
    /// Index into `choices`.
    pub correct: usize,
    pub explanation: String,
}

pub const MAX_CHOICES: usize = 6;

#[derive(Debug)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// Every `*.toml` file in `dir`, in file name order.
pub fn load_dir(dir: &Path) -> Result<Vec<Question>, Error> {
    let io = |e: std::io::Error| Error(format!("{}: {e}", dir.display()));
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(io)? {
        let path = entry.map_err(io)?.path();
        if path.extension().is_some_and(|e| e == "toml") {
            paths.push(path);
        }
    }
    if paths.is_empty() {
        return Err(Error(format!(
            "{}: no *.toml question files",
            dir.display()
        )));
    }
    paths.sort();
    paths
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path)
                .map_err(|e| Error(format!("{}: {e}", path.display())))?;
            parse(&text).map_err(|Error(e)| Error(format!("{}: {e}", path.display())))
        })
        .collect()
}

pub fn parse(text: &str) -> Result<Question, Error> {
    let q: File = toml::from_str(text).map_err(|e| Error(e.to_string()))?;
    let fail = |msg: String| Err(Error(msg));
    if q.question.trim().is_empty() {
        return fail("question text is empty".into());
    }
    if !(2..=MAX_CHOICES).contains(&q.choices.len()) {
        return fail(format!(
            "needs between 2 and {MAX_CHOICES} choices, has {}",
            q.choices.len()
        ));
    }
    for (j, c) in q.choices.iter().enumerate() {
        if c.trim().is_empty() {
            return fail(format!("choice {} is empty", j + 1));
        }
        if q.choices[..j].contains(c) {
            return fail(format!("choice {c:?} is listed twice"));
        }
    }
    let Some(correct) = q.choices.iter().position(|c| *c == q.answer) else {
        return fail(format!("answer {:?} is not one of the choices", q.answer));
    };
    if q.explanation.trim().is_empty() {
        return fail("explanation is empty".into());
    }
    Ok(Question {
        text: q.question,
        choices: q.choices,
        correct,
        explanation: q.explanation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answer_resolves_to_choice_index() {
        let q = parse(
            "question = \"q\"\nchoices = [\"a\", \"b\", \"c\"]\nanswer = \"c\"\nexplanation = \"e\"\n",
        )
        .unwrap();
        assert_eq!(q.correct, 2);
    }

    #[test]
    fn rejects_broken_questions() {
        let cases = [
            (
                "answer not a choice",
                "choices = [\"a\", \"b\"]\nanswer = \"B\"\nexplanation = \"e\"",
            ),
            (
                "single choice",
                "choices = [\"a\"]\nanswer = \"a\"\nexplanation = \"e\"",
            ),
            (
                "duplicate choice",
                "choices = [\"a\", \"a\"]\nanswer = \"a\"\nexplanation = \"e\"",
            ),
            (
                "typoed key",
                "choices = [\"a\", \"b\"]\nanswer = \"a\"\nexplanation = \"e\"\nsecnods = 3",
            ),
            ("no explanation", "choices = [\"a\", \"b\"]\nanswer = \"a\""),
            (
                "empty explanation",
                "choices = [\"a\", \"b\"]\nanswer = \"a\"\nexplanation = \" \"",
            ),
        ];
        for (what, body) in cases {
            let text = format!("question = \"q\"\n{body}\n");
            assert!(parse(&text).is_err(), "{what} was accepted");
        }
        let valid =
            "question = \"q\"\nchoices = [\"a\", \"b\"]\nanswer = \"a\"\nexplanation = \"e\"\n";
        assert!(
            parse(valid).is_ok(),
            "the cases above differ from this only in their flaw"
        );
    }

    #[test]
    fn directory_loads_toml_files_and_names_the_broken_one() {
        let dir = std::env::temp_dir().join(format!("nixcon-quiz-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, text: &str| std::fs::write(dir.join(name), text).unwrap();
        write("README.md", "not a question");
        assert!(load_dir(&dir).is_err(), "directory without questions");

        write(
            "a.toml",
            "question = \"first\"\nchoices = [\"x\", \"y\"]\nanswer = \"y\"\nexplanation = \"e\"\n",
        );
        write(
            "b.toml",
            "question = \"second\"\nchoices = [\"x\", \"y\"]\nanswer = \"x\"\nexplanation = \"e\"\n",
        );
        let questions = load_dir(&dir).unwrap();
        assert_eq!(questions.len(), 2);

        write(
            "c.toml",
            "question = \"third\"\nchoices = [\"x\", \"y\"]\nanswer = \"z\"\nexplanation = \"e\"\n",
        );
        let err = load_dir(&dir).unwrap_err().to_string();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(err.contains("c.toml"), "{err}");
    }
}
