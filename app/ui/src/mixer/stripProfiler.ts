import { createContext, type ProfilerOnRenderCallback } from "react";

export const StripProfiler = createContext<ProfilerOnRenderCallback | undefined>(undefined);
