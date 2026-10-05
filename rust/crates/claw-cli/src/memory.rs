use std::env;

use runtime::{LearningReport, MemoryEntry, MemoryError, MemoryKind, MemoryStore, TurnSummary};

const MEMORY_FILE_NAME: &str = "memory.json";
const MAX_REPORT_ENTRIES: usize = 40;

fn store() -> Result<MemoryStore, MemoryError> {
    let cwd = env::current_dir()?;
    MemoryStore::load(cwd.join(".claw").join(MEMORY_FILE_NAME))
}

pub fn context_for(input: &str) -> Result<Option<String>, MemoryError> {
    store().map(|store| store.context_for(input, 5))
}

pub fn learn_and_save(
    input: &str,
    summary: &TurnSummary,
) -> Result<LearningReport, MemoryError> {
    let mut store = store()?;
    let report = store.learn_from_turn(input, summary);
    if report.created > 0 || report.reinforced > 0 {
        store.save()?;
    }
    Ok(report)
}

pub fn render(args: Option<&str>) -> Result<String, MemoryError> {
    let raw = args.unwrap_or("list").trim();
    if raw.is_empty() || raw == "list" {
        return render_list(&store()?);
    }

    let mut parts = raw.splitn(2, char::is_whitespace);
    let action = parts.next().unwrap_or_default();
    let remainder = parts.next().map(str::trim).unwrap_or_default();

    match action {
        "search" => {
            if remainder.is_empty() {
                return Ok("Memory search requires a query. Usage: /memory search <query>".to_string());
            }
            render_search(&store()?, remainder)
        }
        "forget" => {
            if remainder.is_empty() {
                return Ok("Memory forget requires an id. Usage: /memory forget <id>".to_string());
            }
            let mut store = store()?;
            if !store.forget(remainder) {
                return Ok(format!("Memory entry '{remainder}' was not found."));
            }
            store.save()?;
            Ok(format!("Forgot memory entry {remainder}."))
        }
        "clear" => {
            if remainder != "--confirm" {
                return Ok(
                    "Memory clear requires confirmation. Run /memory clear --confirm to remove all learned project memory."
                        .to_string(),
                );
            }
            let mut store = store()?;
            let removed = store.clear();
            store.save()?;
            Ok(format!("Cleared {removed} learned memory entries."))
        }
        _ => Ok(
            "Usage: /memory | /memory list | /memory search <query> | /memory forget <id> | /memory clear --confirm"
                .to_string(),
        ),
    }
}

fn render_list(store: &MemoryStore) -> Result<String, MemoryError> {
    let mut entries = store.entries().to_vec();
    entries.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.use_count.cmp(&left.use_count))
    });

    let mut lines = vec![
        "Learned memory".to_string(),
        format!("  Storage          {}", store.path().display()),
        format!("  Entries          {}", entries.len()),
        "  Scope            project-local".to_string(),
    ];

    if entries.is_empty() {
        lines.push(String::new());
        lines.push("  No learned memories yet.".to_string());
        lines.push("  Complete a tool-using task and Claw will record an experience, strategy, or failure lesson.".to_string());
        return Ok(lines.join("\n"));
    }

    lines.push(String::new());
    lines.push("Recent memories".to_string());
    for entry in entries.into_iter().take(MAX_REPORT_ENTRIES) {
        lines.push(format_memory_entry(&entry));
    }
    if store.entries().len() > MAX_REPORT_ENTRIES {
        lines.push(format!(
            "  ... {} more entries; use /memory search <query> to narrow the list.",
            store.entries().len() - MAX_REPORT_ENTRIES
        ));
    }
    Ok(lines.join("\n"))
}

fn render_search(store: &MemoryStore, query: &str) -> Result<String, MemoryError> {
    let matches = store.search(query, 10);
    let mut lines = vec![
        format!("Memory search · {query}"),
        format!("  Matches          {}", matches.len()),
    ];
    if matches.is_empty() {
        lines.push("  No relevant learned memory found.".to_string());
        return Ok(lines.join("\n"));
    }

    lines.push(String::new());
    for result in matches {
        lines.push(format!(
            "  [{:.2}] {}",
            result.score,
            format_memory_entry(&result.entry)
        ));
    }
    Ok(lines.join("\n"))
}

fn format_memory_entry(entry: &MemoryEntry) -> String {
    format!(
        "  {} · {} · confidence={:.2} · usefulness={:.2} · uses={} · {}",
        entry.id,
        memory_kind_label(entry.kind),
        entry.confidence,
        entry.usefulness,
        entry.use_count,
        entry.content
    )
}

fn memory_kind_label(kind: MemoryKind) -> &'static str {
    kind.as_str()
}

#[cfg(test)]
mod tests {
    use super::memory_kind_label;
    use runtime::MemoryKind;

    #[test]
    fn renders_memory_kind_labels() {
        assert_eq!(memory_kind_label(MemoryKind::Strategy), "strategy");
    }
}
