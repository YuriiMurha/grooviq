// The fixture needs Profiler callbacks in Vite's optimized E2E build, too.
declare module "react-dom/profiling" {
  export { createRoot } from "react-dom/client";
}
