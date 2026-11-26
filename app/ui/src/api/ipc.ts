import { invoke } from "@tauri-apps/api/core";
import type { CommandMap } from "./generated/contract";

export function invokeCommand<K extends keyof CommandMap>(
  command: K,
  ...args: CommandMap[K]["args"] extends undefined ? [] : [CommandMap[K]["args"]]
): Promise<CommandMap[K]["reply"]> {
  return invoke(command, args[0]);
}
