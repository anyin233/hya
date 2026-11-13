use super::*;

fn group_skill_spellings(skills: &[SkillCatalogEntry]) -> Vec<(SkillCatalogEntry, Vec<String>)> {
    let mut groups: BTreeMap<String, (SkillCatalogEntry, BTreeSet<String>)> = BTreeMap::new();
    for skill in skills {
        let key = skill.path.to_string_lossy().into_owned();
        let entry = groups
            .entry(key)
            .or_insert_with(|| (skill.clone(), BTreeSet::new()));
        entry.1.insert(skill.name.clone());
    }
    let mut out = groups
        .into_values()
        .map(|(entry, names)| {
            let spellings = ordered_skill_spellings(names);
            let mut entry = entry;
            if let Some(primary) = spellings.first() {
                entry.name = primary.clone();
            }
            (entry, spellings)
        })
        .collect::<Vec<_>>();
    out.sort_by(|left, right| left.0.name.cmp(&right.0.name));
    out
}

fn ordered_skill_spellings(names: BTreeSet<String>) -> Vec<String> {
    let mut shorts = names
        .iter()
        .filter(|name| !name.contains(':'))
        .cloned()
        .collect::<Vec<_>>();
    shorts.sort();
    let mut qualified = names
        .into_iter()
        .filter(|name| name.contains(':'))
        .collect::<Vec<_>>();
    qualified.sort();
    shorts.extend(qualified);
    shorts
}

fn format_spelling_list(spellings: &[String]) -> String {
    match spellings {
        [] => String::new(),
        [only] => only.clone(),
        [primary, rest @ ..] => format!("{primary} (also: {})", rest.join(", ")),
    }
}

pub(super) fn skill_index_with_spellings(skills: &[SkillCatalogEntry]) -> Option<String> {
    let groups = group_skill_spellings(skills);
    if groups.is_empty() {
        return None;
    }
    let mut section =
        "These skills are available on demand; read the named SKILL.md when relevant:".to_string();
    for (entry, spellings) in groups {
        section.push_str("\n- ");
        section.push_str(&format_spelling_list(&spellings));
        section.push_str(": ");
        section.push_str(&entry.description);
    }
    Some(section)
}

pub(super) fn inline_selected_skills_with_spellings(skills: &[SkillCatalogEntry]) -> String {
    let mut section =
        "The following skills are embedded in full because no skill tool is available:".to_string();
    for (entry, spellings) in group_skill_spellings(skills) {
        section.push_str("\n\n## ");
        section.push_str(&format_spelling_list(&spellings));
        section.push('\n');
        section.push_str(&entry.description);
        section.push_str("\n\n");
        section.push_str(&entry.content);
    }
    section
}
