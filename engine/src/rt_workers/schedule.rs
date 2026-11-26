use super::{CapacityError, JobId, PoolConfig, filled};

/// A schedule exceeds its configured capacity or contains an invalid edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScheduleError {
    Capacity(CapacityError),
    Order { job: JobId, dependency: JobId },
}

/// Prepared dependency lists and execution levels. The caller supplies canonical job IDs.
pub struct Schedule {
    offsets: Vec<usize>,
    edges: Vec<(JobId, JobId)>,
    level_jobs: Vec<JobId>,
    widths: Vec<usize>,
    max_width: usize,
}

impl Schedule {
    /// Every edge is `(job, dependency)`. Job IDs range from zero to `jobs - 1`.
    pub fn build(
        jobs: usize,
        deps: &[(JobId, JobId)],
        config: &PoolConfig,
    ) -> Result<Self, ScheduleError> {
        use ScheduleError::Capacity;
        if jobs > config.max_jobs || u32::try_from(jobs).is_err() {
            return Err(Capacity(CapacityError::Jobs));
        }
        if deps.len() > config.max_deps {
            return Err(Capacity(CapacityError::Dependencies));
        }
        for &(job, dependency) in deps {
            if job as usize >= jobs || dependency >= job {
                return Err(ScheduleError::Order { job, dependency });
            }
        }

        let mut edges = Vec::new();
        edges
            .try_reserve_exact(deps.len())
            .map_err(|_| Capacity(CapacityError::Allocation))?;
        edges.extend_from_slice(deps);
        edges.sort_unstable();

        let offsets_len = jobs.checked_add(1).ok_or(Capacity(CapacityError::Jobs))?;
        let mut offsets = filled(offsets_len, || 0).map_err(Capacity)?;
        for &(job, _) in &edges {
            offsets[job as usize + 1] += 1;
        }
        let mut levels = filled(jobs, || 0).map_err(Capacity)?;
        let mut widths = filled(jobs, || 0).map_err(Capacity)?;
        let mut level_count = 0;
        for job in 0..jobs {
            offsets[job + 1] += offsets[job];
            let level = edges[offsets[job]..offsets[job + 1]]
                .iter()
                .map(|&(_, dependency)| levels[dependency as usize] + 1)
                .max()
                .unwrap_or(0);
            levels[job] = level;
            widths[level] += 1;
            level_count = level_count.max(level + 1);
        }
        widths.truncate(level_count);

        let mut level_next = filled(level_count, || 0).map_err(Capacity)?;
        for level in 1..level_count {
            level_next[level] = level_next[level - 1] + widths[level - 1];
        }
        let mut level_jobs = filled(jobs, || 0).map_err(Capacity)?;
        for (job, level) in levels.into_iter().enumerate() {
            level_jobs[level_next[level]] = job as JobId;
            level_next[level] += 1;
        }
        let max_width = widths.iter().copied().max().unwrap_or(0);
        Ok(Self {
            offsets,
            edges,
            level_jobs,
            widths,
            max_width,
        })
    }

    pub fn jobs(&self) -> usize {
        self.level_jobs.len()
    }

    pub fn edges(&self) -> usize {
        self.edges.len()
    }

    pub fn level_widths(&self) -> &[usize] {
        &self.widths
    }

    pub fn max_width(&self) -> usize {
        self.max_width
    }

    pub(super) fn levels(&self) -> impl Iterator<Item = &[JobId]> {
        let mut start = 0;
        self.widths.iter().map(move |&width| {
            let end = start + width;
            let level = &self.level_jobs[start..end];
            start = end;
            level
        })
    }

    pub(super) fn dependencies(&self, job: usize) -> impl Iterator<Item = usize> {
        self.edges[self.offsets[job]..self.offsets[job + 1]]
            .iter()
            .map(|&(_, dependency)| dependency as usize)
    }
}
