import { useState, type CSSProperties } from 'react';
import { Knob } from '../src/devices/controls/Knob';
import { Toggle } from '../src/devices/controls/Toggle';
import { Segmented } from '../src/devices/controls/Segmented';
import { Knob as RackKnob } from '../src/rack/Knob';
import { RecordButton } from '../src/transport/RecordButton';
import { KeyboardToggle } from '../src/transport/KeyboardToggle';
import { MetronomeButton } from '../src/transport/MetronomeButton';
import { useLatest, type ParamControl } from '../src/devices/controls/gesture';
import { references, stateContract, themes } from './contract';
import type { ReactNode } from 'react';

function Reference({ id, children }: { id: string; children: ReactNode }) {
  const reference = references.find((item) => item.id === id);
  if (!reference) throw new Error(`Unknown gallery reference ${id}`);
  return <figure className="gallery-reference" data-reference={id}>
    <figcaption>{reference.title}</figcaption>{children}
  </figure>;
}

// Local preview only: no engine, project history, persistence or cancellation-proof claim.
function Numeric({ field = false }: { field?: boolean }) {
  const [value, setValue] = useState(0.5);
  const latest = useLatest(value);
  const control: ParamControl = {
    set: setValue,
    begin: () => {
      const origin = latest.current;
      let alive = true;
      return {
        alive: () => alive,
        input: (next) => { if (!alive) return false; setValue(next); return true; },
        commit: (next) => { if (alive) { alive = false; setValue(next); } },
        cancel: () => { if (alive) { alive = false; setValue(origin); } },
      };
    },
  };
  return <Knob id={field ? 'gallery-field' : 'gallery-level'}
    control={control} param={{ min: 0, max: 1, default: 0.5, curve: 'lin', stepped: false }}
    value={value} label={field ? 'Amount' : 'Level'} name={field ? 'Gallery amount' : 'Gallery level'}
    does="Adjusts the generated gallery value." format={(n) => `${Math.round(n * 100)}%`}
    variant={field ? 'field' : 'knob'} />;
}

export function Gallery() {
  const [enabled, setEnabled] = useState(false);
  const [wave, setWave] = useState(0);
  const [volume, setVolume] = useState(0.5);
  const [recording, setRecording] = useState(false);
  const [typing, setTyping] = useState(false);
  const [metronome, setMetronome] = useState(false);
  return <main className="gallery" data-reference-status="candidate" data-theme="riso">
    <header><h1>Grooviq control reference</h1><p>Riso Ink · Manrope · generated local values</p>
      <p>Live production controls. Captures are reference candidates until reviewed.</p></header>
    <section aria-labelledby="numeric-heading" data-case="device-numeric" style={{ '--kc': 'var(--dev-amp)' } as CSSProperties}>
      <h2 id="numeric-heading">Device parameters</h2>
      <p>Actual rotary and field variants. Tab for focus; hover or edit with the existing gestures.</p>
      <div className="gallery-strip"><Reference id="device.parameter.knob"><Numeric /></Reference>
        <Reference id="device.parameter.field"><Numeric field /></Reference></div>
    </section>
    <section aria-labelledby="choice-heading" data-case="device-choice" style={{ '--kc': 'var(--dev-osc1)' } as CSSProperties}>
      <h2 id="choice-heading">Device choices</h2>
      <div className="gallery-strip"><Reference id="device.switch"><Toggle id="gallery-power" label="Gallery power" does="Switches generated gallery state." on={enabled} onChange={setEnabled} /></Reference>
        <Reference id="device.choice">
        <Segmented id="gallery-wave" name="Gallery waveform" options={['Sine', 'Triangle', 'Saw']} min={0} value={wave}
          does="Chooses a generated waveform setting." onChange={setWave} /></Reference></div>
    </section>
    <section aria-labelledby="rack-heading" data-case="rack-knob" style={{ '--ink': 'var(--ink-2)' } as CSSProperties}>
      <h2 id="rack-heading">Rack parameter</h2>
      <div className="gallery-strip"><Reference id="rack.parameter.knob"><RackKnob label="Gallery rack volume" value={volume} min={0} max={1} step={0.01} defaultValue={0.5}
        format={(n) => `${Math.round(n * 100)}%`} hint="Adjusts generated rack volume." onCommit={async (n) => { setVolume(n); }} /></Reference></div>
    </section>
    <section aria-labelledby="transport-heading" data-case="transport">
      <h2 id="transport-heading">Transport switches</h2>
      <div className="gallery-strip"><Reference id="transport.record"><RecordButton recording={recording} onToggle={() => setRecording(!recording)} /></Reference>
        <Reference id="transport.typing-keyboard"><KeyboardToggle on={typing} onToggle={() => setTyping(!typing)} /></Reference>
        <Reference id="transport.metronome"><MetronomeButton on={metronome} onToggle={() => setMetronome(!metronome)} /></Reference></div>
      <p>These controls change local state only. No sound or keyboard instrument is connected.</p>
    </section>
    <footer><h2>Reference coverage</h2><p>{stateContract.disabled}</p><p>{stateContract.error}</p>
      <p>Whole-screen errors, repair, Undo, window lifetime and compact workspace geometry have separate app-fixture coverage.</p>
      <p>{themes.map((theme) => `${theme.name}: ${theme.status}`).join(' · ')}</p>
      <p>Built-in controls only. Plugin hosting is not represented.</p></footer>
  </main>;
}
