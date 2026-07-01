//! Diagnostic: score a recognized phrase against every main sentence of a
//! recorded script, to see WHY the aligner picked a given index.
//! Usage: cargo run -p prompter-core --example score -- <recording.jsonl> "<phrase>"
use prompter_core::{script, similarity};
use std::io::BufRead;

fn main() {
    let mut a = std::env::args().skip(1);
    let path = a.next().expect("recording path");
    let phrase = a.next().expect("phrase");
    let f = std::fs::File::open(&path).unwrap();
    let first = std::io::BufReader::new(f).lines().next().unwrap().unwrap();
    let v: serde_json::Value = serde_json::from_str(&first).unwrap();
    let src = v.get("source").and_then(|s| s.as_str()).unwrap();
    let parsed = script::parse(src).unwrap();
    let mut sents: Vec<String> = Vec::new();
    for sec in &parsed.sections {
        for el in &sec.elements {
            if let prompter_core::script::Element::Text(ss) = el {
                for s in ss {
                    sents.push(s.text.clone());
                }
            }
        }
    }
    let mut scored: Vec<(usize, f32)> = sents
        .iter()
        .enumerate()
        .map(|(i, s)| (i, similarity(&phrase, s)))
        .collect();
    scored.sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap());
    println!("phrase: {phrase:?}");
    for (i, sc) in scored.iter().take(8) {
        println!("  idx {i:>2}  score {sc:.3}  {}", &sents[*i][..sents[*i].len().min(70)]);
    }
}
