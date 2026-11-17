import { useEffect, useRef, useState } from "react";
import { midiInputs, midiSetEnabled, onMidiInputsChanged, type MidiInput } from "../api/notes";
import { errorText } from "../api/project";

/** The MIDI keyboards plugged in, each with a switch: a switched-off keyboard is ignored. The
 * list follows plugging and unplugging while the popover is open. */
export function MidiSettings() {
  const [inputs, setInputs] = useState<MidiInput[] | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  /** The newest request: an older list that arrives late must not overwrite a newer state. */
  const newest = useRef(0);

  useEffect(() => {
    let gone = false;
    let off: (() => void) | undefined;
    const load = () => {
      const mine = ++newest.current;
      midiInputs()
        .then((list) => {
          if (!gone && mine === newest.current) setInputs(list);
        })
        .catch((e: unknown) => {
          if (!gone) setProblem(errorText(e));
        });
    };
    load();
    void onMidiInputsChanged(load).then((unlisten) => {
      if (gone) unlisten();
      else off = unlisten;
    });
    return () => {
      gone = true;
      off?.();
    };
  }, []);

  const show = (name: string, enabled: boolean) =>
    setInputs((list) => list?.map((i) => (i.name === name ? { ...i, enabled } : i)) ?? list);

  const toggle = async (name: string, enabled: boolean) => {
    newest.current += 1;
    show(name, enabled);
    try {
      await midiSetEnabled(name, enabled);
      setProblem(null);
    } catch (e) {
      // The shell kept the old state.
      show(name, !enabled);
      setProblem(errorText(e));
    }
  };

  return (
    <fieldset className="devices midi">
      <legend>MIDI keyboards</legend>
      {inputs?.length === 0 && (
        <p className="midi-empty">No MIDI keyboard found. Plug one in and it appears here.</p>
      )}
      {inputs?.map((input) => (
        <label
          key={input.name}
          className="device"
          data-hint="Plays this MIDI keyboard on the selected channel. Switch it off to ignore it."
        >
          <input
            type="checkbox"
            role="switch"
            checked={input.enabled}
            onChange={(e) => void toggle(input.name, e.target.checked)}
          />
          <span className="device-name">{input.name}</span>
        </label>
      ))}
      {problem && (
        <p className="midi-problem" role="alert">
          {problem}
        </p>
      )}
    </fieldset>
  );
}
