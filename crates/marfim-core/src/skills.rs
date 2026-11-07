//! Skills: SKILL.md frontmatter loading from `.marfim/skills` and
//! `.claude/skills` (compat). allowed-tools restricts the tool surface.

use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub allowed_tools: Option<Vec<String>>,
    pub body: String,
    pub path: std::path::PathBuf,
}

type FmTuple = (String, String, Option<Vec<String>>);

fn parse_frontmatter(text: &str) -> Option<(FmTuple, String)> {
    let rest = text.strip_prefix("---")?;
    let (fm, body) = rest.split_once("---")?;
    let mut name = None;
    let mut desc = None;
    let mut tools = None;
    for line in fm.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("name:") {
            name = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("description:") {
            desc = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("allowed-tools:") {
            let list: Vec<String> = v
                .trim()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            tools = Some(list);
        }
    }
    Some((
        (name?, desc.unwrap_or_default(), tools),
        body.trim().to_string(),
    ))
}

pub fn load_skill(path: &Path) -> crate::Result<Skill> {
    let text = std::fs::read_to_string(path)?;
    let ((name, description, allowed_tools), body) = parse_frontmatter(&text).ok_or_else(|| {
        crate::CoreError::Config(format!(
            "invalid SKILL.md frontmatter at {}",
            path.display()
        ))
    })?;
    Ok(Skill {
        name,
        description,
        allowed_tools,
        body,
        path: path.to_path_buf(),
    })
}

/// Discover skills under a directory tree (one subdir per skill).
pub fn discover(root: &Path) -> Vec<Skill> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    let mut dirs: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    dirs.sort();
    for d in dirs {
        let skill_md = d.join("SKILL.md");
        if skill_md.is_file() {
            if let Ok(s) = load_skill(&skill_md) {
                out.push(s);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter_and_body() {
        let md =
            "---\nname: tidy\ndescription: Tidy up\nallowed-tools: [Read, Grep]\n---\nBody text.";
        let ((n, d, t), b) = parse_frontmatter(md).unwrap();
        assert_eq!(n, "tidy");
        assert_eq!(d, "Tidy up");
        assert_eq!(t, Some(vec!["Read".into(), "Grep".into()]));
        assert_eq!(b, "Body text.");
    }

    #[test]
    fn invalid_frontmatter_is_none() {
        assert!(parse_frontmatter("no frontmatter here").is_none());
    }

    #[test]
    fn discovers_sorted_skills() {
        let root = marfim_testkit::unique_dir("marfim-skills");
        let _ = std::fs::remove_dir_all(&root);
        for n in ["b-skill", "a-skill"] {
            let d = root.join(n);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join("SKILL.md"),
                format!("---\nname: {n}\ndescription: d\n---\nbody"),
            )
            .unwrap();
        }
        let skills = discover(&root);
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "a-skill");
    }
}
