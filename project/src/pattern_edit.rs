//! Control-side pattern operations. Apply to a candidate, admit it, then publish atomically.
use crate::{ClipContent, Pattern, Project};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PatternEdit {
    Create { name: String },
    Duplicate { pattern: u32 },
    Rename { pattern: u32, name: String },
    Select { pattern: u32 },
    Delete { pattern: u32 },
    MakeUnique { clip: u32 },
}

/// Pure candidate mutation; callers must discard the candidate on error.
pub fn apply(project: &mut Project, edit: PatternEdit) -> Result<u32, String> {
    let find = |p: &Project, id| {
        p.patterns
            .iter()
            .position(|p| p.id == id)
            .ok_or_else(|| format!("Pattern {id} no longer exists. Reload the pattern list."))
    };
    match edit {
        PatternEdit::Create { name } => {
            let id = project.take_id().ok_or("Project ids are exhausted.")?;
            project.patterns.push(Pattern::new(id, name));
            Ok(id)
        }
        PatternEdit::Duplicate { pattern } => {
            let mut copy = project.patterns[find(project, pattern)?].clone();
            let id = project.take_id().ok_or("Project ids are exhausted.")?;
            copy.id = id;
            copy.revision = 1;
            project.patterns.push(copy);
            Ok(id)
        }
        PatternEdit::Rename { pattern, name } => {
            let index = find(project, pattern)?;
            project.patterns[index].name = name;
            Ok(pattern)
        }
        PatternEdit::Select { pattern } => {
            find(project, pattern)?;
            project.current_pattern = pattern;
            Ok(pattern)
        }
        PatternEdit::Delete { pattern } => {
            let index = find(project, pattern)?;
            if project.patterns.len() == 1 {
                return Err("Keep at least one pattern.".into());
            }
            if project.arrangement.clips.iter().any(
                |c| matches!(c.content, ClipContent::Pattern { pattern: id, .. } if id == pattern),
            ) {
                return Err("Remove this pattern's playlist clips before deleting it.".into());
            }
            project.patterns.remove(index);
            if project.current_pattern == pattern {
                project.current_pattern = project.patterns[0].id;
            }
            Ok(project.current_pattern)
        }
        PatternEdit::MakeUnique { clip } => {
            let index = project
                .arrangement
                .clips
                .iter()
                .position(|c| c.id == clip)
                .ok_or("Clip no longer exists.")?;
            let ClipContent::Pattern { pattern, .. } = project.arrangement.clips[index].content
            else {
                return Err("Choose a pattern clip.".into());
            };
            let id = apply(project, PatternEdit::Duplicate { pattern })?;
            if let ClipContent::Pattern { pattern, .. } =
                &mut project.arrangement.clips[index].content
            {
                *pattern = id;
            }
            Ok(id)
        }
    }
}
