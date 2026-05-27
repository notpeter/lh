use crate::common::ThreadSummary;

#[derive(Debug)]
pub enum MatchResult<'a> {
    None,
    One(&'a ThreadSummary),
    Ambiguous(Vec<&'a ThreadSummary>),
}

pub fn select_thread<'a>(threads: &'a [ThreadSummary], query: Option<&str>) -> MatchResult<'a> {
    if threads.is_empty() {
        return MatchResult::None;
    }

    let Some(query) = query.map(str::trim).filter(|query| !query.is_empty()) else {
        return MatchResult::One(&threads[0]);
    };

    let mut scored = threads
        .iter()
        .filter_map(|thread| {
            let score = score_thread(thread, query);
            (score > 0).then_some((score, thread))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| right.updated_sort_key().cmp(&left.updated_sort_key()))
    });

    let Some((top_score, top)) = scored.first().copied() else {
        return MatchResult::None;
    };

    let ambiguous = scored
        .iter()
        .take_while(|(score, _)| top_score.saturating_sub(*score) <= 8)
        .map(|(_, thread)| *thread)
        .collect::<Vec<_>>();
    if ambiguous.len() > 1 {
        if top
            .id
            .to_ascii_lowercase()
            .starts_with(&query.to_ascii_lowercase())
        {
            MatchResult::One(top)
        } else {
            MatchResult::Ambiguous(ambiguous)
        }
    } else {
        MatchResult::One(top)
    }
}

fn score_thread(thread: &ThreadSummary, query: &str) -> u32 {
    let fields = [
        Some(thread.id.as_str()),
        thread.name.as_deref(),
        thread.preview.as_deref(),
        thread.source_path.as_ref().and_then(|path| path.to_str()),
    ];

    fields
        .into_iter()
        .flatten()
        .map(|field| score_field(field, query))
        .max()
        .unwrap_or_default()
}

fn score_field(field: &str, query: &str) -> u32 {
    let field = field.to_ascii_lowercase();
    let query = query.to_ascii_lowercase();

    if field == query {
        return 1000;
    }
    if field.starts_with(&query) {
        return 900 + query.len() as u32;
    }
    if field.contains(&query) {
        return 600 + query.len() as u32;
    }

    let mut score = 0;
    let mut last_index = 0usize;
    for ch in query.chars() {
        let Some(index) = field[last_index..].find(ch) else {
            return 0;
        };
        score += 10;
        if index == 0 {
            score += 2;
        }
        last_index += index + ch.len_utf8();
    }
    score
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::common::AgentKind;

    use super::*;

    fn thread(id: &str, name: &str) -> ThreadSummary {
        ThreadSummary {
            agent: AgentKind::Codex,
            id: id.to_string(),
            name: Some(name.to_string()),
            cwd: PathBuf::from("/tmp"),
            created_at: None,
            updated_at: None,
            source_path: None,
            preview: None,
            removable: None,
            resume_hint: None,
        }
    }

    #[test]
    fn id_prefix_wins() {
        let threads = vec![thread("abc123", "other"), thread("def456", "abc")];
        assert!(matches!(
            select_thread(&threads, Some("abc1")),
            MatchResult::One(thread) if thread.id == "abc123"
        ));
    }

    #[test]
    fn close_name_matches_are_ambiguous() {
        let threads = vec![thread("1", "fix parser"), thread("2", "fix parsing")];
        assert!(matches!(
            select_thread(&threads, Some("fix pars")),
            MatchResult::Ambiguous(_)
        ));
    }
}
