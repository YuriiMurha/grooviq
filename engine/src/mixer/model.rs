macro_rules! id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub u64);
    };
}
id!(MixerTrackId);
id!(InsertId);
id!(SendId);
/// Identity belongs to the source, never its current destination or vector position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceId {
    Channel(u64),
    AudioTrack(u64),
    /// Engine-owned inputs with no user identity: they can never collide with a channel or
    /// a playlist track, whatever IDs those use.
    System(SystemSource),
}
/// The test tone and the legacy built-in synth (the `Command::Note` instrument).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SystemSource {
    Tone,
    Synth,
}
pub const MASTER: MixerTrackId = MixerTrackId(0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Insert {
    pub id: InsertId,
    /// Declared upper bound; actual device preparation must verify this later.
    pub latency_frames: u32,
    pub memory_bytes: u64,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: MixerTrackId,
    /// Master alone has no output. Every other track has one post-fader main output.
    pub output: Option<MixerTrackId>,
    /// Signal order is this vector's order, not insert ID order.
    pub inserts: Vec<Insert>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tap {
    /// After inserts, before fader.
    PreFader,
    PostFader,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Send {
    pub id: SendId,
    pub from: MixerTrackId,
    pub to: MixerTrackId,
    pub tap: Tap,
    /// Linear amplitude in [0, 4] (silence through approximately +12 dB).
    /// Zero remains a topology dependency: changing gain cannot introduce a cycle.
    pub gain: f32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sidechain {
    pub from: MixerTrackId,
    pub to: InsertId,
    pub tap: Tap,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub id: SourceId,
    pub destination: MixerTrackId,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mixer {
    pub tracks: Vec<Track>,
    pub sends: Vec<Send>,
    pub sidechains: Vec<Sidechain>,
    pub sources: Vec<Source>,
}
/// Caller policy, not a per-track insert ceiling. All bounds are inclusive.
/// Compiler scratch/output reservation and future stereo buffers are charged before
/// topology copies. Device preparation must separately enforce declared device bounds.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub nodes: u64,
    pub edges: u64,
    pub buffers: u64,
    pub latency_frames: u64,
    pub memory_bytes: u64,
    pub block_frames: u32,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            nodes: 16_384,
            edges: 65_536,
            buffers: 131_072,
            latency_frames: 1_048_576,
            memory_bytes: 512 * 1024 * 1024,
            block_frames: 64,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Resources {
    pub nodes: u64,
    pub edges: u64,
    pub buffers: u64,
    /// Conservative sum of all declared insert latencies, bounds every DAG path.
    pub latency_frames: u64,
    pub memory_bytes: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    Nodes,
    Edges,
    Buffers,
    Latency,
    Memory,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompileError {
    Overflow,
    Budget {
        resource: Limit,
        required: u64,
        limit: u64,
    },
    InvalidBlockSize,
    MissingMaster,
    DuplicateTrack(MixerTrackId),
    DuplicateInsert(InsertId),
    DuplicateSend(SendId),
    DuplicateSource(SourceId),
    DuplicateSidechain,
    DanglingTrack(MixerTrackId),
    DanglingInsert(InsertId),
    InvalidOutput(MixerTrackId),
    InvalidGain(SendId),
    SelfEdge(MixerTrackId),
    MasterOutgoing,
    Cycle,
}
