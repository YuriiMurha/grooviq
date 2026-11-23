type Reference = Readonly<{
  id: string;
  title: string;
  source: string;
  role: "slider" | "switch" | "radio" | "button";
  name: string;
  behavior: string;
}>;

/** Reuse roles and state meaning across styles; only Riso currently has app tokens. */
export const references: readonly Reference[] = [
  { id: "device.parameter.knob", title: "Rotary parameter", source: "src/devices/controls/Knob.tsx",
    role: "slider", name: "Gallery level", behavior: "Arrow keys, Home/End, fine drag, wheel and reset; local preview values." },
  { id: "device.parameter.field", title: "Numeric parameter", source: "src/devices/controls/Knob.tsx",
    role: "slider", name: "Gallery amount", behavior: "Field variant of the same parameter control and gestures." },
  { id: "device.switch", title: "Device switch", source: "src/devices/controls/Toggle.tsx",
    role: "switch", name: "Gallery power", behavior: "Click or Enter toggles the parameter; aria-checked communicates state." },
  { id: "device.choice", title: "Device choice", source: "src/devices/controls/Segmented.tsx",
    role: "radio", name: "Sine", behavior: "One Tab stop; arrow keys select and move focus within the group." },
  { id: "rack.parameter.knob", title: "Rack parameter", source: "src/rack/Knob.tsx",
    role: "slider", name: "Gallery rack volume", behavior: "Pointer release commits; keys and reset are individual edits." },
  { id: "transport.record", title: "Record", source: "src/transport/RecordButton.tsx",
    role: "button", name: "Record", behavior: "Pressed state uses aria-pressed; this fixture changes local state only." },
  { id: "transport.typing-keyboard", title: "Typing keyboard", source: "src/transport/KeyboardToggle.tsx",
    role: "button", name: "Typing keyboard", behavior: "Pressed state uses aria-pressed; no instrument is connected here." },
  { id: "transport.metronome", title: "Metronome", source: "src/transport/MetronomeButton.tsx",
    role: "button", name: "Metronome", behavior: "Pressed state uses aria-pressed; no audio is connected here." },
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
  { id: "riso", name: "Riso Ink", status: "implemented" },
  { id: "taste", name: "Taste", status: "proposed" },
  { id: "luminous", name: "Luminous", status: "proposed" },
  { id: "industrial", name: "Industrial", status: "proposed" },
  { id: "slate-system", name: "Slate System", status: "proposed" },
  { id: "console", name: "Console", status: "proposed" },
] as const;
