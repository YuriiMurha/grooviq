import type { FamilyId } from "./catalog";

type Reference = Readonly<{
  id: string;
  family: FamilyId;
  title: string;
  source: string;
  role: "slider" | "switch" | "radio" | "button" | "textbox";
  name: string;
  exact?: boolean;
  behavior: string;
}>;

/** Reuse roles and state meaning across styles; only Riso currently has app tokens. */
export const references: readonly Reference[] = [
  { id: "device.parameter.knob", family: "device.parameter", title: "Rotary parameter", source: "src/devices/controls/Knob.tsx",
    role: "slider", name: "Gallery level", behavior: "Arrow keys, Home/End, fine drag, wheel and reset; local preview values." },
  { id: "device.parameter.field", family: "device.parameter", title: "Numeric parameter", source: "src/devices/controls/Knob.tsx",
    role: "slider", name: "Gallery amount", behavior: "Field variant of the same parameter control and gestures." },
  { id: "device.switch", family: "device.choice", title: "Device switch", source: "src/devices/controls/Toggle.tsx",
    role: "switch", name: "Gallery power", behavior: "Click or Enter toggles the parameter; aria-checked communicates state." },
  { id: "device.choice", family: "device.choice", title: "Device choice", source: "src/devices/controls/Segmented.tsx",
    role: "radio", name: "Sine", behavior: "One Tab stop; arrow keys select and move focus within the group." },
  { id: "rack.parameter.knob", family: "rack.parameter", title: "Rack parameter", source: "src/rack/Knob.tsx",
    role: "slider", name: "Gallery rack volume", behavior: "Pointer release commits; keys and reset are individual edits." },
  { id: "transport.record", family: "transport.action", title: "Record", source: "src/transport/RecordButton.tsx",
    role: "button", name: "Record", behavior: "Pressed state uses aria-pressed; this fixture changes local state only." },
  { id: "transport.typing-keyboard", family: "transport.action", title: "Typing keyboard", source: "src/transport/KeyboardToggle.tsx",
    role: "button", name: "Typing keyboard", behavior: "Pressed state uses aria-pressed; no instrument is connected here." },
  { id: "transport.metronome", family: "transport.action", title: "Metronome", source: "src/transport/MetronomeButton.tsx",
    role: "button", name: "Metronome", behavior: "Pressed state uses aria-pressed; no audio is connected here." },
  { id: "transport.tempo", family: "transport.tempo", title: "Tempo entry", source: "src/transport/BpmField.tsx",
    role: "textbox", name: "BPM", behavior: "Real tempo entry with a local accepted-value and commit-count witness; no engine/project writes." },
  { id: "output.status", family: "output.status", title: "Output status", source: "src/output/OutputChip.tsx",
    role: "button", name: "No output", behavior: "Real output chip with generated device props. Opener callbacks are recorded; no output/MIDI dialog or hardware is mounted." },
];

export const stateContract = {
  normal: "Initial generated values and unpressed switches.",
  "keyboard-focus": "Real sequential Tab focus, including :focus-visible.",
  hover: "Real pointer over the existing control; hover paint is component-specific.",
  compact: "The same semantic states at 900×560; no replacement control markup.",
  disabled: "Not applicable: these exported APIs do not accept a disabled state. App busy/admission flows have separate coverage.",
  error: "Not applicable: these leaf APIs do not own error presentation. App repair/refusal surfaces have separate coverage.",
} as const;

export const themes = [
  { id: "riso", name: "Riso Ink", status: "implemented", tokens: "src/theme/riso.css" },
  { id: "taste", name: "Taste", status: "proposed", tokens: null },
  { id: "luminous", name: "Luminous", status: "proposed", tokens: null },
  { id: "industrial", name: "Industrial", status: "proposed", tokens: null },
  { id: "slate-system", name: "Slate System", status: "proposed", tokens: null },
  { id: "console", name: "Console", status: "proposed", tokens: null },
] as const;
