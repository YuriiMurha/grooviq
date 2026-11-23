type State = "normal" | "keyboard-focus" | "hover" | "compact" | "disabled" | "error"
  | "selected" | "pressed" | "editing" | "warning" | "empty" | "open" | "busy" | "dragging";

type Family = Readonly<{
  id: string;
  title: string;
  sources: readonly string[];
  semantics: string;
  states: readonly State[];
  notApplicable: Readonly<Partial<Record<State, string>>>;
  specimenIds: readonly string[];
  referenceScope: "isolated-local-state" | "app-fixture" | "not-implemented";
}>;

const leafExclusions = {
  disabled: "The exported leaf APIs do not accept disabled; admission/busy belongs to their app owner.",
  error: "The exported leaf APIs do not present errors; owning flows retain refusal and repair coverage.",
} as const;

/** Source inventory, not a capture ledger or an approved pixel baseline. */
export const catalog = [
  { id: "device.parameter", title: "Device parameters", sources: ["src/devices/controls/Knob.tsx", "src/devices/controls/gesture.ts"],
    semantics: "Slider; rotary and field variants share continuous gesture ownership, keys and reset.",
    states: ["normal", "keyboard-focus", "hover", "compact", "dragging"], notApplicable: leafExclusions,
    specimenIds: ["device.parameter.knob", "device.parameter.field"], referenceScope: "isolated-local-state" },
  { id: "device.choice", title: "Device choices", sources: ["src/devices/controls/Segmented.tsx", "src/devices/controls/Toggle.tsx"],
    semantics: "Roving radio group and switch remain separate APIs; do not substitute transport pressed behavior.",
    states: ["normal", "keyboard-focus", "hover", "compact", "selected"], notApplicable: leafExclusions,
    specimenIds: ["device.choice", "device.switch"], referenceScope: "isolated-local-state" },
  { id: "rack.parameter", title: "Rack parameter", sources: ["src/rack/Knob.tsx"],
    semantics: "Slider with a release commit; not the device editor's gesture owner.",
    states: ["normal", "keyboard-focus", "hover", "compact", "dragging"], notApplicable: leafExclusions,
    specimenIds: ["rack.parameter.knob"], referenceScope: "isolated-local-state" },
  { id: "transport.action", title: "Transport switches", sources: ["src/transport/RecordButton.tsx", "src/transport/KeyboardToggle.tsx", "src/transport/MetronomeButton.tsx"],
    semantics: "Named buttons expose pressed state; local specimens do not start recording, MIDI or sound.",
    states: ["normal", "keyboard-focus", "hover", "compact", "pressed"], notApplicable: leafExclusions,
    specimenIds: ["transport.record", "transport.typing-keyboard", "transport.metronome"], referenceScope: "isolated-local-state" },
  { id: "transport.tempo", title: "Tempo entry", sources: ["src/transport/BpmField.tsx", "src/transport/tempoCommit.ts"],
    semantics: "Decimal textbox; Enter/blur commit, Escape discards, keys/wheel/drag request bounded tempo changes.",
    states: ["normal", "keyboard-focus", "hover", "compact", "editing"], notApplicable: {
      disabled: "BpmField has no disabled prop; project replacement admission belongs to its owner.",
      error: "Invalid text is discarded without an error surface; do not invent aria-invalid styling.",
    }, specimenIds: ["transport.tempo"], referenceScope: "isolated-local-state" },
  { id: "output.status", title: "Output status opener", sources: ["src/output/OutputChip.tsx", "src/output/kind.ts"],
    semantics: "32px output button, warning and latency content; passive 24px metadata is a different family.",
    states: ["normal", "keyboard-focus", "hover", "compact", "warning", "empty"], notApplicable: leafExclusions,
    specimenIds: ["output.status"], referenceScope: "isolated-local-state" },
  { id: "output.dialog", title: "Output dialog and guard", sources: ["src/output/OutputPopover.tsx", "src/midi/MidiSettings.tsx", "src/output/focus.ts"],
    semantics: "Dialog, output radio choices and guard checkbox; opening also mounts the MIDI owner.",
    states: ["normal", "keyboard-focus", "hover", "compact", "open", "warning", "error"], notApplicable: {},
    specimenIds: [], referenceScope: "app-fixture" },
  { id: "transport.position", title: "Playback position", sources: ["src/transport/PositionReadout.tsx"],
    semantics: "Timer reads reported live frames; a static picture does not prove timing or latency.",
    states: ["normal", "compact"], notApplicable: { "keyboard-focus": "Read-only timer, not a focus target.", hover: "No hover action.", disabled: "Read-only display.", error: "No leaf error surface." },
    specimenIds: [], referenceScope: "app-fixture" },
  { id: "workspace.metadata", title: "Metadata and pattern length", sources: ["src/layout/Workspace.tsx", "src/rack/ChannelRack.tsx"],
    semantics: "Passive 24px metadata and editable pattern length keep distinct semantics and hit targets.",
    states: ["normal", "compact", "editing", "keyboard-focus"], notApplicable: {},
    specimenIds: [], referenceScope: "app-fixture" },
  { id: "workspace.splitter", title: "Panel chrome and split", sources: ["src/layout/Workspace.tsx", "src/layout/panelLayout.ts"],
    semantics: "40px header and 6px separator; requested preference differs from displayed fit and ARIA bounds.",
    states: ["normal", "keyboard-focus", "hover", "compact", "dragging"], notApplicable: {},
    specimenIds: [], referenceScope: "app-fixture" },
  { id: "rack.channel", title: "Channel row", sources: ["src/rack/ChannelRow.tsx", "src/rack/AddChannelMenu.tsx"],
    semantics: "Selection, mute, solo, missing sample and instrument actions retain their own ownership.",
    states: ["normal", "keyboard-focus", "hover", "compact", "selected", "warning", "open"], notApplicable: {},
    specimenIds: [], referenceScope: "app-fixture" },
  { id: "rack.steps", title: "Step grid", sources: ["src/rack/StepGrid.tsx", "src/rack/glRenderer.ts", "src/rack/canvasRenderer.ts"],
    semantics: "Canvas grid, step velocity and playhead; not a copied grid of DOM buttons.",
    states: ["normal", "keyboard-focus", "hover", "compact", "selected", "empty"], notApplicable: {},
    specimenIds: [], referenceScope: "app-fixture" },
  { id: "roll.editor", title: "Note editor and keys", sources: ["src/roll/PianoRoll.tsx", "src/roll/KeyColumn.tsx", "src/roll/VelocityLane.tsx"],
    semantics: "Notes, selection, velocity and keyboard input; body and canvas each retain the 112px fit contract.",
    states: ["normal", "keyboard-focus", "hover", "compact", "selected", "dragging", "empty"], notApplicable: {},
    specimenIds: [], referenceScope: "app-fixture" },
  { id: "roll.tools", title: "Roll tools and Feel", sources: ["src/roll/RollToolbar.tsx"],
    semantics: "Pressed tools, native choices and the existing Feel subtree; overlay lifecycle is separate app behavior.",
    states: ["normal", "keyboard-focus", "hover", "compact", "disabled", "pressed", "open"], notApplicable: {},
    specimenIds: [], referenceScope: "app-fixture" },
  { id: "device.window", title: "Built-in device windows", sources: ["src/devices/DeviceWindow.tsx", "src/devices/DeviceWindows.tsx"],
    semantics: "Floating built-in dialog, stacking, scaling and opener return; not a modal or plugin host.",
    states: ["normal", "keyboard-focus", "hover", "compact", "open", "dragging"], notApplicable: {},
    specimenIds: [], referenceScope: "app-fixture" },
  { id: "device.displays", title: "Instrument displays", sources: ["src/devices/sampler/WaveformView.tsx", "src/devices/controls/EnvelopeGraph.tsx", "src/devices/synth/OscDisplay.tsx", "src/devices/synth/SynthDisplays.tsx", "src/devices/synth/OutputScope.tsx"],
    semantics: "Reported waveforms, envelope and scope; do not infer live telemetry or DSP from a static picture.",
    states: ["normal", "compact", "empty"], notApplicable: { disabled: "Displays do not accept a disabled control state." },
    specimenIds: [], referenceScope: "app-fixture" },
  { id: "sampler.file-repair", title: "Sampler file and repair", sources: ["src/devices/sampler/SamplerEditor.tsx", "src/devices/sampler/useTrimRepair.ts"],
    semantics: "File details, missing source, trim repair, explicit reset and Undo; generated sample data only.",
    states: ["normal", "keyboard-focus", "hover", "compact", "disabled", "error", "busy", "warning"], notApplicable: {},
    specimenIds: [], referenceScope: "app-fixture" },
  { id: "pattern.picker", title: "Pattern picker", sources: ["src/patterns/PickerRail.tsx"],
    semantics: "Pattern selection and lifecycle; opening the outer rail must preserve the inner workspace node and preference.",
    states: ["normal", "keyboard-focus", "hover", "compact", "selected", "editing", "busy"], notApplicable: {},
    specimenIds: [], referenceScope: "app-fixture" },
  { id: "playlist.clips", title: "Song clips", sources: ["src/playlist/Playlist.tsx", "src/playlist/renderer.ts"],
    semantics: "Actual clip canvas, selection, held preview and admitted selected-clip actions; use settled pixels plus model assertions.",
    states: ["normal", "keyboard-focus", "hover", "compact", "selected", "dragging", "empty", "disabled", "busy"], notApplicable: {},
    specimenIds: [], referenceScope: "app-fixture" },
  { id: "project.menu", title: "Project menu", sources: ["src/project/ProjectMenu.tsx"],
    semantics: "Menu admission, keyboard focus and project actions are owned by the existing project flow.",
    states: ["normal", "keyboard-focus", "hover", "compact", "disabled", "open"], notApplicable: {},
    specimenIds: [], referenceScope: "app-fixture" },
  { id: "project.dialog", title: "Project dialogs", sources: ["src/project/Modal.tsx", "src/project/ConfirmDialog.tsx", "src/project/ExportDialog.tsx"],
    semantics: "Modal focus containment, confirm/cancel, export options and progress; device windows are a different family.",
    states: ["normal", "keyboard-focus", "hover", "compact", "disabled", "error", "busy", "open"], notApplicable: {},
    specimenIds: [], referenceScope: "app-fixture" },
  { id: "project.feedback", title: "Save, hint, log and toast feedback", sources: ["src/project/SaveState.tsx", "src/hint/HintBar.tsx", "src/log/AudioLog.tsx", "src/toast/Toasts.tsx"],
    semantics: "Read-only dirty/saved status, contextual hints and owner-reported warnings/errors.",
    states: ["normal", "compact", "warning", "error", "empty"], notApplicable: { disabled: "Read-only feedback, not an admitted action." },
    specimenIds: [], referenceScope: "app-fixture" },
  { id: "plugin.host", title: "Plugin host window", sources: [], semantics: "No implemented audio-plugin host is represented by built-in device chrome.",
    states: [], notApplicable: {}, specimenIds: [], referenceScope: "not-implemented" },
] as const satisfies readonly Family[];

export type FamilyId = typeof catalog[number]["id"];
export const referencePolicy = {
  direction: "Original Grooviq mocks; inherited Riso Ink, Manrope and semantic token roles.",
  approvedPixelReference: null,
  status: "Candidate captures require separate reviewed reference selection; source mapping is not visual approval.",
  stateScope: "Listed states are source-inventoried. Unlisted states remain unclassified, not implicitly inapplicable or proved.",
  themeFollowUp: "#387",
} as const;
