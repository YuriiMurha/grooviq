import { useState, type CSSProperties } from 'react';
import { Knob } from '../src/devices/controls/Knob';
import { Toggle } from '../src/devices/controls/Toggle';
import { Segmented } from '../src/devices/controls/Segmented';
import { Knob as RackKnob } from '../src/rack/Knob';
import { RecordButton } from '../src/transport/RecordButton';
import { KeyboardToggle } from '../src/transport/KeyboardToggle';
import { MetronomeButton } from '../src/transport/MetronomeButton';
import { BpmField } from '../src/transport/BpmField';
import { OutputChip } from '../src/output/OutputChip';
import type { Output } from '../src/audio';
import { useLatest, type ParamControl } from '../src/devices/controls/gesture';
import { references, stateContract, themes } from './contract';
import { catalog, referencePolicy } from './catalog';
import { gallerySeed } from './seed';
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
  const [value, setValue] = useState<number>(gallerySeed.numeric);
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

function Tempo() {
  const [bpm, setBpm] = useState<number>(gallerySeed.tempo);
  const [commits, setCommits] = useState(0);
  return <>
    <BpmField bpm={bpm} onCommit={async (next) => { setBpm(next); setCommits((count) => count + 1); return true; }} />
    <div className="gallery-fixture-witness">Local accepted value: <output aria-label="Accepted gallery tempo">{bpm.toFixed(2)}</output>
      {' · '}commits: <output aria-label="Gallery tempo commits">{commits}</output></div>
  </>;
}

type OutputMode = "none" | "headphones" | "speakers" | "long-name";
const generatedOutputs: Record<Exclude<OutputMode, "none">, Output> = {
  headphones: { id: "gallery-headphones", name: "Generated headphones", kind: "headphones", isDefault: true, current: true, sampleRate: 48000 },
  speakers: { id: "gallery-speakers", name: "Generated speakers", kind: "speakers", isDefault: false, current: true, sampleRate: 48000 },
  "long-name": { id: "gallery-long", name: "Generated headphones with an intentionally long interface name for compact coverage", kind: "headphones", isDefault: false, current: true, sampleRate: 48000 },
};

function OutputStatus() {
  const [mode, setMode] = useState<OutputMode>(gallerySeed.output);
  const [requests, setRequests] = useState(0);
  return <>
    <OutputChip output={mode === "none" ? null : generatedOutputs[mode]} latencyMs={8} open={false}
      onToggle={() => setRequests((count) => count + 1)} />
    <label className="gallery-scenario">Generated output state
      <select aria-label="Generated output state" value={mode} onChange={(event) => {
        const next = event.currentTarget.value;
        if (next === "none" || next === "headphones" || next === "speakers" || next === "long-name") setMode(next);
      }}>
        <option value="none">No output</option><option value="headphones">Headphones</option>
        <option value="speakers">Speakers warning</option><option value="long-name">Long name</option>
      </select>
    </label>
    <div className="gallery-fixture-witness">Opener requests: <output aria-label="Output opener requests">{requests}</output></div>
    <p className="gallery-fixture-note">Callback recorder only. No output dialog, MIDI settings or device selection is mounted.</p>
  </>;
}

export function Gallery() {
  const [enabled, setEnabled] = useState<boolean>(gallerySeed.switches);
  const [wave, setWave] = useState<number>(gallerySeed.waveIndex);
  const [volume, setVolume] = useState<number>(gallerySeed.numeric);
  const [recording, setRecording] = useState<boolean>(gallerySeed.switches);
  const [typing, setTyping] = useState<boolean>(gallerySeed.switches);
  const [metronome, setMetronome] = useState<boolean>(gallerySeed.switches);
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
    <section aria-labelledby="tempo-output-heading" data-case="tempo-output">
      <h2 id="tempo-output-heading">Tempo and output status</h2>
      <div className="gallery-strip"><Reference id="transport.tempo"><Tempo /></Reference>
        <Reference id="output.status"><OutputStatus /></Reference></div>
    </section>
    <footer><h2>Reference coverage</h2><p>{stateContract.disabled}</p><p>{stateContract.error}</p>
      <p>Whole-screen errors, repair, Undo, window lifetime and compact workspace geometry have separate app-fixture coverage.</p>
      <p>{themes.map((theme) => `${theme.name}: ${theme.status}`).join(' · ')}</p>
      <p>Built-in controls only. Plugin hosting is not represented.</p>
      <details className="gallery-catalog"><summary>Semantic family catalog</summary>
        <p>{referencePolicy.direction} {referencePolicy.status} {referencePolicy.stateScope}</p>
        <dl>{catalog.map((family) => <div key={family.id}>
          <dt>{family.title} <small>{family.referenceScope}</small></dt>
          <dd><p>{family.semantics}</p><p>Applicable states: {family.states.join(', ') || 'not implemented'}.</p>
            <p>Gallery specimens: {family.specimenIds.join(', ') || 'none; app-fixture or capability work remains'}.</p>
            <p>Source: {family.sources.join(', ') || 'not implemented'}.</p>
            {Object.entries(family.notApplicable).map(([state, reason]) => <p key={state}>{state}: {reason}</p>)}
          </dd>
        </div>)}</dl>
      </details></footer>
  </main>;
}
