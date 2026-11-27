import { useEffect, useState } from "react";
import { arrangementCurve, type ArrangementStamp, type CurvePoint } from "../../api/arrangement";
import { errorText } from "../../api/project";
import { projectGeneration } from "../../project/replacement";

type CurveRead = ArrangementStamp & { clip: number; replacements: number; attempt: number; pointCount: number } & (
  | { points: readonly CurvePoint[]; error?: never }
  | { error: string; points?: never }
);

export type AutomationCurve = {
  points?: readonly CurvePoint[];
  error?: string;
  retry: () => void;
};

/** The dock edits a window. Timeline trim needs the complete curve under the same stamp. */
export function useAutomationCurve(stamp: ArrangementStamp | null, clip: number | null, pointCount: number | null): AutomationCurve {
  const generation = stamp?.generation, revision = stamp?.revision;
  const replacements = projectGeneration();
  const [attempt, setAttempt] = useState(0);
  const [read, setRead] = useState<CurveRead | null>(null);
  useEffect(() => {
    if (generation === undefined || revision === undefined || clip === null || pointCount === null) return;
    let active = true;
    const current = () => active && projectGeneration() === replacements;
    const request = { generation, revision, clip, replacements, attempt, pointCount };
    void arrangementCurve(request, clip).then(reply => {
      if (!current()) return;
      if (reply.generation !== generation || reply.revision !== revision || reply.clip !== clip || reply.points.length !== pointCount) {
        setRead({ ...request, error: "The curve changed while loading." });
        return;
      }
      setRead({ ...request, points: reply.points });
    }).catch(error => {
      if (current()) setRead({ ...request, error: errorText(error) });
    });
    return () => { active = false; };
  }, [generation, revision, clip, pointCount, replacements, attempt]);

  const loaded = read && read.generation === generation && read.revision === revision && read.clip === clip &&
    read.replacements === replacements && read.attempt === attempt && read.pointCount === pointCount ? read : null;
  return { points: loaded?.points, error: loaded?.error, retry: () => setAttempt(current => current + 1) };
}
