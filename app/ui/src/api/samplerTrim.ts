import { invoke } from "@tauri-apps/api/core";
import type { TrimView } from "./generated/contract";
import { invokeCommand } from "./ipc";
export type TrimState = TrimView;
export type TrimReply = { status: "applied"; state: TrimState; changed: boolean } | { status: "refused"; state: TrimState | null; message: string };
export type TrimAction = { kind: "view" } | { kind: "reset"; stamp: string } | { kind: "undo"; stamp: string; token: string };
export function samplerTrim(channel: number, action: TrimAction): Promise<TrimReply> {
  if (action.kind === "view") return invokeCommand("sampler_trim_view", { channel }).then(state => ({ status: "applied", state, changed: false }));
  if (action.kind === "reset") return invoke("sampler_trim_reset", { channel, stamp: action.stamp });
  return invoke("sampler_trim_undo", { channel, stamp: action.stamp, token: action.token });
}
