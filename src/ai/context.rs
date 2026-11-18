//! Nachrichten als Kontext für die KI: Kopfzeilen und Text in markierten Blöcken, eine
//! Unterhaltung mit einem Budget an Zeichen (älteste zuerst, jede Nachricht ohne Zitate).

use super::store::Mail;
use super::text::{self, Fence};

/// Eine Nachricht als markierter Block; `text` ist der (schon gekürzte) Inhalt.
pub fn mail_block(fence: &Fence, label: &str, mail: &Mail, text: &str) -> String {
    let mut head = mail.header();
    if mail.is_sent {
        head.push_str("\nRichtung: von mir gesendet");
    }
    fence.wrap(label, &format!("{head}\n\n{}", text.trim()))
}

/// Budget gerecht verteilen: kurze Texte bekommen, was sie brauchen, der Rest geht an die langen.
pub fn share(lengths: &[usize], budget: usize) -> Vec<usize> {
    let mut alloc = vec![0usize; lengths.len()];
    let mut left = budget;
    let mut open: Vec<usize> = (0..lengths.len()).collect();
    while !open.is_empty() && left > 0 {
        let fair = left / open.len();
        if fair == 0 {
            break;
        }
        let mut still = Vec::new();
        for &i in &open {
            let give = (lengths[i] - alloc[i]).min(fair);
            alloc[i] += give;
            left -= give;
            if alloc[i] < lengths[i] {
                still.push(i);
            }
        }
        if still.len() == open.len() {
            break;
        }
        open = still;
    }
    alloc
}

/// Unterhaltung als Blöcke (älteste zuerst). Bei sehr langen Unterhaltungen bleiben die erste
/// und die neuesten Nachrichten; dazwischen steht, wie viele ausgelassen wurden.
pub fn conversation(fence: &Fence, mails: &[Mail], budget: usize, label: &str) -> String {
    const MAX: usize = 30;
    let picked: Vec<(usize, &Mail)> = if mails.len() > MAX {
        std::iter::once((0, &mails[0]))
            .chain(mails.iter().enumerate().skip(mails.len() - (MAX - 1)))
            .collect()
    } else {
        mails.iter().enumerate().collect()
    };
    let texts: Vec<String> = picked.iter().map(|(_, m)| m.own_text()).collect();
    let alloc = share(
        &texts.iter().map(|t| t.chars().count()).collect::<Vec<_>>(),
        budget,
    );
    let mut out = Vec::new();
    let mut previous = None;
    for (k, (i, m)) in picked.iter().enumerate() {
        if let Some(p) = previous {
            if *i > p + 1 {
                out.push(format!(
                    "({} Nachrichten dazwischen ausgelassen)",
                    i - p - 1
                ));
            }
        }
        previous = Some(*i);
        let body = text::truncate(&texts[k], alloc[k].max(200));
        out.push(mail_block(fence, &format!("{label} {}", i + 1), m, &body));
    }
    out.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::share;

    #[test]
    fn budget_is_shared_fairly() {
        assert_eq!(share(&[100, 5000, 5000], 3000), vec![100, 1450, 1450]);
        assert_eq!(share(&[10, 20], 1000), vec![10, 20]);
        assert_eq!(share(&[], 1000), Vec::<usize>::new());
    }
}
