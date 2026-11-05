//! TodoWrite state model + persistence.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Todo {
    pub content: String,
    pub status: String, // pending | in_progress | completed
    pub active_form: String,
}

pub fn validate(todos: &[serde_json::Value]) -> Result<Vec<Todo>, String> {
    let mut out = Vec::new();
    let mut in_progress = 0usize;
    for t in todos {
        let content = t["content"].as_str().ok_or("todo missing `content`")?;
        let status = t["status"].as_str().ok_or("todo missing `status`")?;
        if !matches!(status, "pending" | "in_progress" | "completed") {
            return Err(format!(
                "bad status `{status}` (pending|in_progress|completed)"
            ));
        }
        if status == "in_progress" {
            in_progress += 1;
        }
        out.push(Todo {
            content: content.into(),
            status: status.into(),
            active_form: t["activeForm"].as_str().unwrap_or(content).into(),
        });
    }
    if in_progress > 1 {
        return Err(format!("at most one in_progress todo (got {in_progress})"));
    }
    Ok(out)
}

pub fn render(todos: &[Todo]) -> String {
    if todos.is_empty() {
        return "(no todos)".into();
    }
    let mut s = String::new();
    for (i, t) in todos.iter().enumerate() {
        let mark = match t.status.as_str() {
            "completed" => "[x]",
            "in_progress" => "[~]",
            _ => "[ ]",
        };
        s.push_str(&format!(
            "{}. {} {} ({})\n",
            i + 1,
            mark,
            t.content,
            t.active_form
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_status_values() {
        let ok = validate(&[serde_json::json!({"content": "a", "status": "pending"})]).unwrap();
        assert_eq!(ok.len(), 1);
        assert!(validate(&[serde_json::json!({"content": "a", "status": "done"})]).is_err());
    }

    #[test]
    fn one_in_progress_max() {
        let bad = validate(&[
            serde_json::json!({"content": "a", "status": "in_progress"}),
            serde_json::json!({"content": "b", "status": "in_progress"}),
        ]);
        assert!(bad.is_err());
    }

    #[test]
    fn render_marks() {
        let todos = validate(&[
            serde_json::json!({"content": "a", "status": "completed", "activeForm": "doing a"}),
            serde_json::json!({"content": "b", "status": "in_progress", "activeForm": "doing b"}),
        ])
        .unwrap();
        let r = render(&todos);
        assert!(r.contains("[x] a"));
        assert!(r.contains("[~] b"));
    }
}
