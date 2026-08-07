/// Home-row letters first so the most reachable keys label the first elements.
const ALPHABET: &[u8; 26] = b"asdfghjklqwertyuiopzxcvbnm";

/// Hint labels for n elements: single letters while they suffice, otherwise
/// uniform two-letter combinations so no label is a prefix of another.
pub fn labels(n: usize) -> Vec<String> {
    if n <= ALPHABET.len() {
        return ALPHABET[..n]
            .iter()
            .map(|c| (*c as char).to_string())
            .collect();
    }
    let mut out = Vec::with_capacity(n);
    'outer: for a in ALPHABET {
        for b in ALPHABET {
            if out.len() == n {
                break 'outer;
            }
            out.push(format!("{}{}", *a as char, *b as char));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn short_lists_use_single_letters() {
        assert_eq!(labels(3), vec!["a", "s", "d"]);
        assert_eq!(labels(26).len(), 26);
        assert!(labels(26).iter().all(|l| l.len() == 1));
    }

    #[test]
    fn long_lists_are_uniform_and_prefix_free() {
        let ls = labels(120);
        assert_eq!(ls.len(), 120);
        assert!(ls.iter().all(|l| l.len() == 2));
        let set: HashSet<&String> = ls.iter().collect();
        assert_eq!(set.len(), 120);
    }
}
