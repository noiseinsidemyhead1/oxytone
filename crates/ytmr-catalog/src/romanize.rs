//! Romanisation des paroles (japonais, coréen, chinois).
//!
//! En clair : sous une ligne écrite en kana, en hangul ou en caractères
//! chinois, on affiche sa lecture en lettres latines. Pour le japonais, la
//! meilleure source est le romaji de NetEase (fait à la main, kanji compris) ;
//! sans lui, seuls les kana sont convertis (les kanji demanderaient un
//! dictionnaire de plusieurs dizaines de Mo). Le coréen se convertit par
//! règle (romanisation révisée), le chinois en pinyin avec les tons.

use pinyin::ToPinyin;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Script {
    Japanese,
    Korean,
    Chinese,
}

fn is_kana(c: char) -> bool {
    matches!(c, '\u{3040}'..='\u{30ff}' | '\u{31f0}'..='\u{31ff}' | '\u{ff66}'..='\u{ff9f}')
}

fn is_hangul(c: char) -> bool {
    matches!(c, '\u{ac00}'..='\u{d7a3}' | '\u{1100}'..='\u{11ff}' | '\u{3130}'..='\u{318f}')
}

fn is_han(c: char) -> bool {
    matches!(c, '\u{4e00}'..='\u{9fff}' | '\u{3400}'..='\u{4dbf}' | '\u{f900}'..='\u{faff}')
}

/// Langue de l'ensemble des paroles : un seul kana suffit à dire japonais
/// (sinon les lignes tout en kanji seraient lues comme du chinois).
fn script_of(lines: &[&str]) -> Option<Script> {
    let chars = || lines.iter().flat_map(|l| l.chars());
    if chars().any(is_kana) {
        Some(Script::Japanese)
    } else if chars().any(is_hangul) {
        Some(Script::Korean)
    } else if chars().any(is_han) {
        Some(Script::Chinese)
    } else {
        None
    }
}

/// Lecture de chaque ligne (`None` : rien à romaniser, ligne déjà latine).
pub fn romanize_lines(lines: &[&str]) -> Vec<Option<String>> {
    let Some(script) = script_of(lines) else {
        return vec![None; lines.len()];
    };
    lines
        .iter()
        .map(|line| {
            let needs = line
                .chars()
                .any(|c| is_kana(c) || is_hangul(c) || is_han(c));
            if !needs {
                return None;
            }
            let out = match script {
                Script::Japanese => yomikata_core::kana_to_romaji(line),
                Script::Korean => hangul(line),
                Script::Chinese => chinese(line),
            };
            let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
            (!out.is_empty() && out != line.trim()).then_some(out)
        })
        .collect()
}

/// Romanisation révisée du coréen, syllabe par syllabe (sans les règles
/// d'assimilation entre syllabes : lisible, pas phonétique au mot près).
fn hangul(text: &str) -> String {
    const INITIAL: [&str; 19] = [
        "g", "kk", "n", "d", "tt", "r", "m", "b", "pp", "s", "ss", "", "j", "jj", "ch", "k", "t",
        "p", "h",
    ];
    const VOWEL: [&str; 21] = [
        "a", "ae", "ya", "yae", "eo", "e", "yeo", "ye", "o", "wa", "wae", "oe", "yo", "u", "wo",
        "we", "wi", "yu", "eu", "ui", "i",
    ];
    const FINAL: [&str; 28] = [
        "", "k", "k", "k", "n", "n", "n", "t", "l", "k", "m", "l", "l", "l", "p", "l", "m", "p",
        "p", "t", "t", "ng", "t", "t", "k", "t", "p", "t",
    ];
    let mut out = String::new();
    let mut previous_syllable = false;
    for c in text.chars() {
        let code = c as u32;
        if (0xac00..=0xd7a3).contains(&code) {
            let index = (code - 0xac00) as usize;
            let (initial, vowel, last) = (index / (21 * 28), (index % (21 * 28)) / 28, index % 28);
            // ㄹ en tête de mot se lit plutôt « r » ; après une syllabe aussi.
            let first = if initial == 5 && !previous_syllable {
                "r"
            } else {
                INITIAL[initial]
            };
            out.push_str(first);
            out.push_str(VOWEL[vowel]);
            out.push_str(FINAL[last]);
            previous_syllable = true;
        } else {
            out.push(c);
            previous_syllable = false;
        }
    }
    out
}

/// Pinyin avec les tons, un mot par caractère.
fn chinese(text: &str) -> String {
    let mut out = String::new();
    for (c, reading) in text.chars().zip(text.to_pinyin()) {
        match reading {
            Some(p) => {
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
                out.push_str(p.with_tone());
                out.push(' ');
            }
            None => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn romanizes_each_script() {
        assert_eq!(
            romanize_lines(&["こんにちは", "hello"]),
            vec![Some("konnichiha".to_owned()), None]
        );
        assert_eq!(
            romanize_lines(&["사랑해"]),
            vec![Some("saranghae".to_owned())]
        );
        assert_eq!(romanize_lines(&["你好"]), vec![Some("nǐ hǎo".to_owned())]);
        // Japonais : la ligne en kanji n'est pas lue en pinyin.
        let lines = romanize_lines(&["東京", "とうきょう"]);
        assert_eq!(lines[0], None);
        assert_eq!(romanize_lines(&["only latin"]), vec![None]);
    }
}
