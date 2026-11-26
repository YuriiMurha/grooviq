import type { ArrangementOperation, ArrangementPage, ArrangementReply, ArrangementStamp } from "../api/arrangement";
import type { ProjectView } from "../api/project";
import type { PatternReply, PatternsView } from "../api/patterns";
import type { ProjectStamp } from "../project/context";
import type { AudioImportInput } from "./audio/media";
import { settleProjectEdits, TransitionWork } from "../project/pending";
import { nextMediaTicket } from "./audio/mediaTicket";

type MediaKind = "import" | "relink";
export type PlaylistSnapshot = ArrangementPage & Readonly<{ complete: true; sourcePeriods: Readonly<Record<number, number>> }>;
export type PlaylistState = Readonly<{
  snapshot: PlaylistSnapshot | null;
  busy: boolean;
  requiresRefresh: boolean;
  message: string | null;
  media: MediaKind | null;
  cancelRequested: boolean;
  cancelledMedia: MediaKind | null;
  waveformVersions: ReadonlyMap<number, number>;
}>;
export type PlaylistResult =
  | Readonly<{ status: "applied" | "refused"; reply: ArrangementReply }>
  | Readonly<{ status: "cancelled"; reply?: ArrangementReply }>
  | Readonly<{ status: "stale" | "blocked" | "unsettled" }>
  | Readonly<{ status: "unpublished"; native: "applied" | "unknown"; message: string }>;
export type PlaylistAdapter = Readonly<{
  origin: () => ProjectStamp;
  /** A Save waiting for an admitted ticket is current; New/Open/Restore are not. */
  current: (origin: ProjectStamp) => boolean;
  /** Admission only: includes the existing Save/gesture transition fence. */
  blocked: () => boolean;
  seekBlocked: () => boolean;
  beginTransition: () => () => void;
  enqueueRead: <T>(command: () => Promise<T>) => Promise<T>;
  enqueueMutation: <T>(command: () => Promise<T>) => Promise<T>;
  readPage: (offset: number, limit: number) => Promise<ArrangementPage>;
  readPatterns: () => Promise<PatternsView>;
  selectPattern: (base: PatternsView, pattern: number) => Promise<PatternReply>;
  edit: (stamp: ArrangementStamp, operations: readonly ArrangementOperation[]) => Promise<ArrangementReply>;
  importAudio: (stamp: ArrangementStamp, input: AudioImportInput, ticket: number) => Promise<ArrangementReply>;
  relinkAudio: (stamp: ArrangementStamp, asset: number, path: string, ticket: number) => Promise<ArrangementReply>;
  cancelAudio: (ticket: number) => Promise<boolean>;
  seek: (stamp: ArrangementStamp, tick: number) => Promise<ArrangementReply>;
  readProject: () => Promise<ProjectView>;
  publishProject: (view: ProjectView, origin: ProjectStamp) => boolean;
  changed: (state: PlaylistState) => void;
}>;
type MediaFlight = {
  readonly ticket: number;
  readonly kind: MediaKind;
  readonly origin: ProjectStamp;
  readonly result: Promise<PlaylistResult>;
  cancelRequested: boolean;
};

export const EMPTY_PLAYLIST: PlaylistState = { snapshot: null, busy: false, requiresRefresh: false, message: null,
  media: null, cancelRequested: false, cancelledMedia: null, waveformVersions: new Map() };
// Native sends cancellation as a refused acknowledgement with one of these reasons.
const mediaCancellationReasons = new Set([
  "Media ticket cancelled",
  "Older or cancelled media request",
  "Media preparation refused: Cancelled",
  "Media preparation failed: Cancelled",
  "Media request cancelled or superseded",
]);
const stampEquals = (a: ArrangementStamp, b: ArrangementStamp) => a.generation === b.generation && a.revision === b.revision;
const integer = (n: number, maximum = Number.MAX_SAFE_INTEGER) => Number.isSafeInteger(n) && n >= 0 && n <= maximum;
const messageOf = (error: unknown) => error instanceof Error ? error.message : String(error);
function requireStamp(stamp: ArrangementStamp): void {
  if (!integer(stamp.generation) || !integer(stamp.revision)) throw new Error("Invalid native arrangement stamp");
}
function requireProject(current: () => boolean): void {
  if (!current()) throw new Error("The project changed while reading its arrangement");
}
function header(page: ArrangementPage): string {
  return JSON.stringify([page.tracks, page.audioAssets, page.loopRegion, page.playMode, page.songError, page.pendingRequests, page.warnings]);
}

/** A whole-list read with bounded progress. A drifted page never becomes a partial snapshot. */
export async function readArrangement(read: PlaylistAdapter["readPage"], current: () => boolean, limit = 256): Promise<Omit<PlaylistSnapshot, "sourcePeriods">> {
  if (!integer(limit, 256) || limit === 0) throw new Error("Invalid arrangement page limit");
  requireProject(current);
  const first = structuredClone(await read(0, limit));
  requireProject(current); requireStamp(first);
  if (first.offset !== 0 || !integer(first.total, 100_000) || first.tracks.length > 256 || first.audioAssets.length > 4096) throw new Error("Invalid native arrangement bounds");
  const trackIds = new Set(first.tracks.map(track => track.id));
  if (trackIds.size !== first.tracks.length || [...trackIds].some(id => !integer(id, 0xffffffff))) throw new Error("Invalid native track identities");
  const expectedHeader = header(first);
  const clips: ArrangementPage["clips"][number][] = [];
  const ids = new Set<number>();
  let page = first;
  for (;;) {
    if (!stampEquals(page, first) || page.total !== first.total || page.offset !== clips.length || header(page) !== expectedHeader || page.clips.length !== Math.min(limit, first.total - clips.length)) throw new Error("Arrangement changed between pages; refresh it");
    for (const clip of page.clips) {
      if (!integer(clip.id, 0xffffffff) || ids.has(clip.id) || !trackIds.has(clip.track)) throw new Error("Invalid native clip identity or track");
      ids.add(clip.id); clips.push(clip);
    }
    if (clips.length === first.total) break;
    requireProject(current);
    page = structuredClone(await read(clips.length, limit));
    requireProject(current);
  }
  return { ...first, clips, complete: true };
}

/** Rejection travels through the real mutation lane so its flush also sees a refusal. */
class PlaylistOutcome extends Error {
  constructor(readonly result: PlaylistResult) { super("Playlist command did not publish"); }
}

/** Owns intent/publication only. The Session adapter supplies the existing command lane. */
export class PlaylistController {
  private readonly work = new TransitionWork();
  private value: PlaylistState = EMPTY_PLAYLIST;
  private epoch = 0;
  private waveformVersions: ReadonlyMap<number, number> = new Map();
  private mediaFlight: MediaFlight | null = null;
  constructor(private readonly adapter: PlaylistAdapter) {}
  get state(): PlaylistState { return this.value; }
  unsettled = (): boolean => this.work.unsettled() || this.mediaFlight !== null || this.value.requiresRefresh;
  settle = async (): Promise<boolean> => {
    const settled = await this.work.settle();
    return settled && !this.value.requiresRefresh;
  };
  private set(next: PlaylistState): void { this.value = next; this.adapter.changed(next); }
  private current(origin: ProjectStamp, epoch: number): boolean { return this.epoch === epoch && this.adapter.current(origin); }
  retire = (): void => { this.epoch++; this.set(EMPTY_PLAYLIST); };

  retireMedia = (): void => {
    this.mediaFlight = null;
  };
  replaceMedia = (): void => {
    this.retireMedia();
    this.waveformVersions = new Map();
  };
  settleMedia = async (): Promise<boolean> => {
    await this.mediaFlight?.result;
    return !this.value.requiresRefresh;
  };
  invalidateWaveforms = (): void => {
    // Only relink can replace bytes while keeping the same asset and sample reference.
    this.waveformVersions = new Map([...this.waveformVersions].map(([asset, version]) => [asset, version + 1]));
  };

  importAudio = (expected: ArrangementStamp, input: AudioImportInput): Promise<PlaylistResult> =>
    this.mediaEdit(expected, "import", (stamp, ticket) => this.adapter.importAudio(stamp, input, ticket));

  relinkAudio = (expected: ArrangementStamp, asset: number, path: string): Promise<PlaylistResult> =>
    this.mediaEdit(expected, "relink", (stamp, ticket) => this.adapter.relinkAudio(stamp, asset, path, ticket), asset);

  cancelMedia = async (): Promise<void> => {
    const flight = this.mediaFlight;
    if (!flight || flight.cancelRequested || !this.adapter.current(flight.origin)) return;
    flight.cancelRequested = true;
    this.set({ ...this.value, cancelRequested: true });
    try {
      // False can mean an early tombstone or a published edit. Only the media reply settles it.
      await this.adapter.cancelAudio(flight.ticket);
    } catch {
      if (this.mediaFlight !== flight) return;
      flight.cancelRequested = false;
      this.set({ ...this.value, cancelRequested: false });
    }
  };

  private mediaEdit(
    expected: ArrangementStamp, kind: MediaKind, invoke: (stamp: ArrangementStamp, ticket: number) => Promise<ArrangementReply>,
    relinkedAsset?: number,
  ): Promise<PlaylistResult> {
    if (this.value.busy || this.mediaFlight || this.value.requiresRefresh || this.adapter.blocked() || this.adapter.seekBlocked()) {
      return Promise.resolve({ status: "blocked" });
    }
    if (!this.value.snapshot || !stampEquals(expected, this.value.snapshot)) return Promise.resolve({ status: "stale" });
    requireStamp(expected);
    const stamp = { generation: expected.generation, revision: expected.revision };
    const origin = { ...this.adapter.origin() };
    if (stamp.generation !== origin.generation || !this.adapter.current(origin)) return Promise.resolve({ status: "stale" });

    const current = () => this.mediaFlight === flight && this.adapter.current(origin);
    const run = async (): Promise<PlaylistResult> => {
      let invoked = false;
      let applied = false;
      try {
        const finish = this.adapter.beginTransition();
        let decoding: Promise<ArrangementReply>;
        try {
          const settled = await settleProjectEdits(this.settleMedia);
          const isCurrent = current();
          if (!settled || !isCurrent || !this.value.snapshot || !stampEquals(stamp, this.value.snapshot)) {
            if (flight.cancelRequested) {
              if (isCurrent) this.set({ ...this.value, cancelledMedia: kind });
              return { status: "cancelled" };
            }
            return { status: settled ? "stale" : "unsettled" };
          }
          invoked = true;
          decoding = invoke(stamp, flight.ticket);
        } finally {
          finish();
        }
        // Allow replacement while decoding. Only the final readback enters the Session lane.
        const reply = await decoding;
        return await this.adapter.enqueueRead(async () => {
          if (!current()) return { status: "stale" };
          requireStamp(reply);
          if (reply.generation !== stamp.generation || reply.revision < stamp.revision) throw new Error("Invalid native audio acknowledgement");
          if (reply.status === "refused") {
            const cancelled = flight.cancelRequested && mediaCancellationReasons.has(reply.message ?? "");
            const requiresRefresh = reply.reload && reply.revision !== stamp.revision;
            const cancelledMessage = requiresRefresh ? `The project changed during the audio ${kind}. Refresh the playlist.` : null;
            this.set({ ...this.value, requiresRefresh,
              cancelledMedia: cancelled ? kind : null,
              message: cancelled ? cancelledMessage : reply.message ?? (kind === "import" ? "The audio import was refused" : "The audio relink was refused") });
            return { status: cancelled ? "cancelled" : "refused", reply };
          }
          if (reply.status !== "applied" || !reply.changed || reply.revision <= stamp.revision) throw new Error("Invalid native audio outcome");
          applied = true;
          const { snapshot } = await this.observe(current);
          if (snapshot.generation !== stamp.generation || snapshot.revision < reply.revision) throw new Error("The audio edit is not yet observable");
          const view = await this.adapter.readProject();
          if (!current()) return { status: "stale" };
          if (view.generation !== origin.generation || !this.adapter.publishProject(view, origin)) throw new Error("The audio edit could not be published");
          if (relinkedAsset !== undefined) {
            const version = (this.waveformVersions.get(relinkedAsset) ?? 0) + 1;
            this.waveformVersions = new Map(this.waveformVersions).set(relinkedAsset, version);
          }
          this.set({ ...this.value, snapshot, requiresRefresh: false, message: null, waveformVersions: this.waveformVersions });
          return { status: "applied", reply };
        });
      } catch (error) {
        if (!current()) return { status: "stale" };
        const message = messageOf(error);
        this.set({ ...this.value, requiresRefresh: invoked, message });
        return invoked ? { status: "unpublished", native: applied ? "applied" : "unknown", message } : { status: "unsettled" };
      } finally {
        if (this.mediaFlight === flight) {
          this.retireMedia();
          this.set({ ...this.value, media: null, cancelRequested: false });
        }
      }
    };

    const flight: MediaFlight = { kind, origin, ticket: nextMediaTicket(),
      result: Promise.resolve().then(run), cancelRequested: false };
    this.mediaFlight = flight;
    this.set({ ...this.value, media: kind, cancelRequested: false, cancelledMedia: null, message: null });
    return flight.result;
  }

  private async observe(current: () => boolean): Promise<{ snapshot: PlaylistSnapshot; patterns: PatternsView }> {
    const snapshot = await readArrangement(this.adapter.readPage, current);
    const sources = await this.adapter.readPatterns();
    requireProject(current);
    if (sources.generation !== snapshot.generation || sources.arrangementRevision !== snapshot.revision || !Array.isArray(sources.patterns)) throw new Error("Pattern sources changed while reading the playlist; refresh it");
    const sourcePeriods: Record<number, number> = {};
    for (const source of sources.patterns) {
      if (!integer(source.id, 0xffffffff) || !integer(source.revision, 0xffffffff) || !integer(source.steps, 256) || source.steps === 0 || Object.hasOwn(sourcePeriods, source.id)) throw new Error("Invalid native pattern source metadata");
      sourcePeriods[source.id] = source.steps * 240;
    }
    if (snapshot.clips.some(clip => clip.content.type === "pattern" && !Object.hasOwn(sourcePeriods, clip.content.pattern))) throw new Error("A playlist pattern source is unavailable; refresh it");
    return { snapshot: { ...snapshot, sourcePeriods }, patterns: sources };
  }

  private async selectUniqueSource(clipId: number, reply: ArrangementReply, current: () => boolean) {
    const { snapshot, patterns } = await this.observe(current);
    const [pattern] = reply.createdIds;
    const target = snapshot.clips.find(clip => clip.id === clipId);
    if (!reply.changed || reply.createdIds.length !== 1 || !integer(pattern, 0xffffffff) ||
        snapshot.generation !== reply.generation || snapshot.revision !== reply.revision ||
        target?.content.type !== "pattern" || target.content.pattern !== pattern ||
        Object.hasOwn(this.value.snapshot?.sourcePeriods ?? {}, pattern)) {
      throw new Error("The unique pattern could not be confirmed. Refresh the playlist.");
    }
    const selection = await this.adapter.selectPattern(patterns, pattern);
    requireProject(current);
    if (selection.status === "refused") throw new Error(selection.message);
    const selected = selection.state;
    if (selected.generation !== reply.generation || selected.selected !== pattern ||
        selected.view.generation !== reply.generation || selected.view.pattern.id !== pattern ||
        selected.arrangementRevision < snapshot.revision) {
      throw new Error("The unique pattern selection could not be confirmed. Refresh the playlist.");
    }
    return { pattern, revision: selected.arrangementRevision };
  }

  reload = async (): Promise<boolean> => {
    if (this.value.busy || this.adapter.blocked()) return false;
    const origin = { ...this.adapter.origin() }, epoch = this.epoch;
    const recoverPublication = this.value.requiresRefresh;
    this.set({ ...this.value, busy: true });
    try {
      return await this.adapter.enqueueRead(async () => {
        const current = () => this.current(origin, epoch);
        const { snapshot } = await this.observe(current);
        if (snapshot.generation !== origin.generation) throw new Error("The project changed before the playlist loaded");
        if (recoverPublication) {
          const view = await this.adapter.readProject();
          requireProject(current);
          if (view.generation !== origin.generation || !this.adapter.publishProject(view, origin)) throw new Error("The playlist view could not be published");
        }
        this.set({ ...this.value, snapshot, busy: true, requiresRefresh: false, message: null, cancelledMedia: null,
          media: this.mediaFlight?.kind ?? null, cancelRequested: this.mediaFlight?.cancelRequested ?? false, waveformVersions: this.waveformVersions });
        return true;
      });
    } catch (error) {
      // A failed observation invalidates editing, but does not invent unsaved native work.
      // An existing unpublished-edit obligation remains latched in requiresRefresh.
      if (this.current(origin, epoch)) this.set({ ...this.value, snapshot: null, message: messageOf(error) });
      return false;
    } finally {
      if (this.current(origin, epoch)) this.set({ ...this.value, busy: false });
    }
  };

  seek = async (expected: ArrangementStamp, tick: number): Promise<void> => {
    const blocked = () => this.adapter.blocked() || this.adapter.seekBlocked();
    if (this.value.busy || this.mediaFlight || this.value.requiresRefresh || blocked()) return;
    if (!this.value.snapshot || !stampEquals(expected, this.value.snapshot)) return;
    requireStamp(expected);
    if (!integer(tick, 38_400_000)) return;
    const captured = { ...expected }, origin = { ...this.adapter.origin() }, epoch = this.epoch;
    const current = () => this.current(origin, epoch) && captured.generation === origin.generation &&
      !!this.value.snapshot && stampEquals(captured, this.value.snapshot);
    if (!current()) return;
    try {
      await this.adapter.enqueueRead(async () => {
        if (!current() || blocked()) return;
        const reply = await this.adapter.seek(captured, tick);
        if (!current()) return;
        requireStamp(reply);
        if (reply.status === "refused") {
          this.set({ ...this.value, message: reply.message ?? "Seek refused" });
          return;
        }
        if (reply.status !== "applied" || !stampEquals(reply, captured) || reply.changed) throw new Error("Invalid native seek acknowledgement");
        this.set({ ...this.value, message: null });
      });
    } catch (error) {
      if (current()) this.set({ ...this.value, message: messageOf(error) });
    }
  };

  mutate = (expected: ArrangementStamp, operations: readonly ArrangementOperation[]): Promise<PlaylistResult> => {
    if (this.value.busy || this.mediaFlight || this.value.requiresRefresh || this.adapter.blocked()) return Promise.resolve({ status: "blocked" });
    if (!this.value.snapshot || !stampEquals(expected, this.value.snapshot)) return Promise.resolve({ status: "stale" });
    requireStamp(expected);
    if (operations.length === 0) return Promise.resolve({ status: "blocked" });
    const captured = { ...expected }, origin = { ...this.adapter.origin() }, epoch = this.epoch;
    if (captured.generation !== origin.generation || !this.current(origin, epoch)) return Promise.resolve({ status: "stale" });
    const batch = structuredClone(operations);
    const unique = batch.length === 1 && batch[0].kind === "makeUnique" ? batch[0] : null;
    // run() registers this unsaved intent synchronously before the first await or native I/O.
    return this.work.run(async (): Promise<PlaylistResult> => {
      this.set({ ...this.value, busy: true, message: null, cancelledMedia: null });
      let finish: (() => void) | null = null;
      let invoked = false;
      let applied = false;
      try {
        finish = this.adapter.beginTransition();
        // Exclude the registered wrapper itself, not work.settle (a different function).
        if (!await settleProjectEdits(this.settle)) return { status: "unsettled" };
        return await this.adapter.enqueueMutation<PlaylistResult>(async () => {
          if (!this.current(origin, epoch) || !this.value.snapshot || !stampEquals(captured, this.value.snapshot)) throw new PlaylistOutcome({ status: "stale" });
          invoked = true;
          const reply = await this.adapter.edit(captured, batch);
          if (!this.current(origin, epoch)) throw new PlaylistOutcome({ status: "stale" });
          requireStamp(reply);
          if (reply.generation !== captured.generation || reply.revision < captured.revision) throw new Error("Invalid native playlist acknowledgement");
          if (reply.status === "refused") {
            this.set({ ...this.value, requiresRefresh: reply.reload, message: reply.message ?? "The playlist edit was refused" });
            throw new PlaylistOutcome({ status: "refused", reply });
          }
          if (reply.status !== "applied" || (reply.changed && reply.revision === captured.revision)) throw new Error("Invalid native playlist outcome");
          applied = true;
          const current = () => this.current(origin, epoch);
          try {
            // Save waits for this ticket, including the source switch after an accepted clone.
            const selection = unique ? await this.selectUniqueSource(unique.clip, reply, current) : null;
            const { snapshot } = await this.observe(current);
            if (snapshot.generation !== captured.generation || snapshot.revision < reply.revision) throw new Error("Playlist acknowledgement is not yet observable");
            const view = await this.adapter.readProject();
            if (!current()) throw new PlaylistOutcome({ status: "stale" });
            if (selection && (snapshot.revision !== selection.revision || view.pattern.id !== selection.pattern)) {
              throw new Error("The unique pattern changed before publication. Refresh the playlist.");
            }
            if (view.generation !== origin.generation || !this.adapter.publishProject(view, origin)) throw new Error("The accepted playlist edit could not be published");
            this.set({ ...this.value, snapshot, busy: true, requiresRefresh: false, message: null, waveformVersions: this.waveformVersions });
            return { status: "applied", reply };
          } catch (error) {
            if (!unique || !current()) throw error;
            // The clone already committed. Let its explicit readback resolve the obligation
            // instead of recording a second, unrelated refusal in the shared command queue.
            const message = messageOf(error);
            this.set({ ...this.value, requiresRefresh: true, message });
            return { status: "unpublished", native: "applied", message };
          }
        });
      } catch (error) {
        if (error instanceof PlaylistOutcome) return error.result;
        if (!this.current(origin, epoch)) return { status: "stale" };
        const message = messageOf(error);
        this.set({ ...this.value, requiresRefresh: invoked, message });
        return invoked ? { status: "unpublished", native: applied ? "applied" : "unknown", message } : { status: "unsettled" };
      } finally {
        finish?.();
        if (this.current(origin, epoch)) this.set({ ...this.value, busy: false });
      }
    }, result => result.status === "applied" ||
      (unique !== null && result.status === "unpublished" && result.native === "applied"));
  };
}
