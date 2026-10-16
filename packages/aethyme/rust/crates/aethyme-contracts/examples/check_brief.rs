//! Validate a decision file until a CLI surface exists (#658).
//!
//! ```sh
//! cargo run -q -p aethyme-contracts --example check_brief -- brief.json
//! ```
//!
//! Prints the token count and the brief record's ID, or every problem with its
//! path. Exits 1 when the brief is invalid, 2 when the file cannot be read.

use aethyme_contracts::experimental_v0::brief::{Brief, MAX_BRIEF_TOKENS, TOKEN_PROFILE};

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: check_brief <decision-file.json>");
        std::process::exit(2);
    };
    let bytes = std::fs::read(&path).unwrap_or_else(|error| {
        eprintln!("{path}: {error}");
        std::process::exit(2);
    });
    match Brief::from_decision_file(&bytes) {
        Ok(brief) => {
            let (_, id) = brief.to_record();
            println!(
                "{path}: ok, {} of {MAX_BRIEF_TOKENS} tokens ({TOKEN_PROFILE}), record {id}",
                brief.token_count()
            );
        }
        Err(errors) => {
            eprint!("{path}: {errors}");
            std::process::exit(1);
        }
    }
}
