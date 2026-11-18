//! Control-side pattern operations. Apply to a candidate, admit it, then publish atomically.
use crate::{ClipContent, Pattern, Project};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PatternEdit {
    Settle,
    Create { name: String },
    Duplicate { pattern: u32 },
    Rename { pattern: u32, name: String },
    Select { pattern: u32 },
    Delete { pattern: u32 },
    MakeUnique { clip: u32 },
}

/// Pure candidate mutation; callers must discard the candidate on error.
pub fn apply(project: &mut Project, edit: PatternEdit) -> Result<u32, PatternError> {
    let find = |p: &Project, id| {
        p.patterns
            .iter()
            .position(|p| p.id == id)
            .ok_or(PatternError::UnknownPattern(id))
    };
    match edit {
        PatternEdit::Settle => Ok(project.current_pattern),
        PatternEdit::Create { name } => {
            let id = project.take_id().ok_or(PatternError::IdExhausted)?;
            project.patterns.push(Pattern::new(id, name));
            Ok(id)
        }
        PatternEdit::Duplicate { pattern } => {
            let mut copy = project.patterns[find(project, pattern)?].clone();
            let id = project.take_id().ok_or(PatternError::IdExhausted)?;
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
                return Err(PatternError::LastPattern);
            }
            if project.arrangement.clips.iter().any(
                |c| matches!(c.content, ClipContent::Pattern { pattern: id, .. } if id == pattern),
            ) {
                return Err(PatternError::ReferencedPattern);
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
                .ok_or(PatternError::UnknownClip)?;
            let ClipContent::Pattern { pattern, .. } = project.arrangement.clips[index].content
            else {
                return Err(PatternError::NotPatternClip);
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

/// Stable wire refusal codes; messages may change without changing the protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RefusalReason {
    ProjectReplaced,
    StaleRevision,
    UnknownPattern,
    UnknownClip,
    NotPatternClip,
    LastPattern,
    ReferencedPattern,
    IdExhausted,
    EngineBusy,
    FileBudget,
    Validation,
    RecordingSettling,
    PendingRecording,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatternError {
    UnknownPattern(u32),
    UnknownClip,
    NotPatternClip,
    LastPattern,
    ReferencedPattern,
    IdExhausted,
}
impl PatternError {
    pub fn reason(self) -> RefusalReason {
        match self {
            Self::UnknownPattern(_) => RefusalReason::UnknownPattern,
            Self::UnknownClip => RefusalReason::UnknownClip,
            Self::NotPatternClip => RefusalReason::NotPatternClip,
            Self::LastPattern => RefusalReason::LastPattern,
            Self::ReferencedPattern => RefusalReason::ReferencedPattern,
            Self::IdExhausted => RefusalReason::IdExhausted,
        }
    }
}
impl std::fmt::Display for PatternError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownPattern(id) => {
                write!(f, "Pattern {id} no longer exists. Reload the pattern list.")
            }
            Self::UnknownClip => write!(f, "Clip no longer exists."),
            Self::NotPatternClip => write!(f, "Choose a pattern clip."),
            Self::LastPattern => write!(f, "Keep at least one pattern."),
            Self::ReferencedPattern => write!(
                f,
                "Remove this pattern's playlist clips before deleting it."
            ),
            Self::IdExhausted => write!(f, "Project ids are exhausted."),
        }
    }
}
impl std::error::Error for PatternError {}
